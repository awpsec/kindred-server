//! Codex browser worker. Kindred owns candidates and action arguments; the
//! official Decisions transport selects only a grounded current choice.
mod driver;
#[cfg(test)]
mod tests;
pub mod transport;

use crate::{
    db::{self, Bot, Run},
    runtime::App,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future, pin::Pin, time::Duration};

pub type Work<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// The official adapter owns supported API-key authentication and returns only
/// a choice ID. Never import Codex subscription tokens into a guessed endpoint.
pub trait Decisions: Send + Sync {
    fn choose<'a>(&'a self, input: &'a DecisionInput) -> Work<'a, String>;
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub goal: String,
    pub origin: String,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    #[serde(default = "default_steps")]
    pub max_actions: usize,
}
fn default_steps() -> usize {
    32
}
impl Task {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.goal.trim().is_empty() && self.goal.len() <= 8000,
            "Invalid browser goal"
        );
        let url = reqwest::Url::parse(&self.origin)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.origin().ascii_serialization() == self.origin,
            "Supply an exact HTTP(S) origin without credentials or a path"
        );
        ensure!(
            (1..=64).contains(&self.max_actions),
            "Browser action limit must be 1..64"
        );
        ensure!(
            self.values.len() <= 16
                && self.values.iter().all(|(key, value)| !key.is_empty()
                    && key.len() <= 64
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    && value.len() <= 16000)
                && self.values.values().map(String::len).sum::<usize>() <= 32000,
            "Browser supplied values exceed their limits"
        );
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub label: String,
    pub kind: String,
    #[serde(default)]
    pub value_key: Option<String>,
}
impl Candidate {
    fn terminal(&self) -> bool {
        matches!(self.kind.as_str(), "finish" | "escalate")
    }
    fn scope(&self) -> &'static str {
        // Checkbox changes and field events may autosave. Unknown clicks may
        // submit remotely. Only these two observation actions are read-only.
        if matches!(self.kind.as_str(), "scroll" | "wait") {
            "routine_vm"
        } else {
            "external"
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub snapshot_id: String,
    pub url: String,
    pub title: String,
    pub text: String,
    pub candidates: Vec<Candidate>,
    pub image: String,
}
impl Observation {
    fn validate(&self, task: &Task) -> Result<()> {
        ensure!(
            !self.snapshot_id.is_empty()
                && self.snapshot_id.len() <= 80
                && self.url.len() <= 8000
                && self.title.len() <= 2000
                && self.text.len() <= 48000
                && self.image.starts_with("data:image/png;base64,")
                && self.image.len() <= 7 * 1024 * 1024,
            "Invalid browser observation"
        );
        let url = reqwest::Url::parse(&self.url)?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.origin().ascii_serialization() == task.origin,
            "Browser left the requested origin"
        );
        ensure!(
            (2..=256).contains(&self.candidates.len()),
            "Invalid browser candidate count"
        );
        let mut ids = std::collections::HashSet::new();
        for candidate in &self.candidates {
            ensure!(
                !candidate.id.is_empty()
                    && candidate.id.len() <= 80
                    && candidate.label.len() <= 2000
                    && ids.insert(&candidate.id)
                    && matches!(
                        candidate.kind.as_str(),
                        "click"
                            | "fill"
                            | "check"
                            | "select"
                            | "scroll"
                            | "wait"
                            | "finish"
                            | "escalate"
                    )
                    && candidate
                        .value_key
                        .as_ref()
                        .is_none_or(|key| task.values.contains_key(key)),
                "Invalid browser candidate"
            );
        }
        ensure!(
            self.candidates.iter().any(|c| c.kind == "finish")
                && self.candidates.iter().any(|c| c.kind == "escalate"),
            "Missing browser exit choices"
        );
        Ok(())
    }
}
#[derive(Serialize)]
pub struct DecisionInput {
    pub goal: String,
    pub values: BTreeMap<String, String>,
    pub observation: Observation,
    pub previous_actions: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReceipt {
    pub applied: bool,
    pub uncertain: bool,
    pub detail: String,
}
pub trait Browser: Send {
    fn observe<'a>(&'a mut self, task: &'a Task) -> Work<'a, Observation>;
    fn act<'a>(
        &'a mut self,
        observation: &'a Observation,
        candidate: &'a Candidate,
    ) -> Work<'a, ActionReceipt>;
}
trait Guard {
    fn check(&self) -> Result<()>;
    fn authorize<'a>(&'a self, task: &'a Task, candidate: &'a Candidate) -> Work<'a, bool>;
    fn requested(&self, candidate: &Candidate) -> Result<String>;
    fn receipt(&self, id: &str, candidate: &Candidate, receipt: &ActionReceipt) -> Result<()>;
}

#[derive(Serialize)]
struct Outcome {
    status: &'static str,
    reason: String,
    actions: Vec<Value>,
    uncertain: bool,
    #[serde(skip)]
    observation: Option<Observation>,
}
impl Outcome {
    fn new(status: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
            actions: vec![],
            uncertain: false,
            observation: None,
        }
    }
    fn result(mut self) -> Value {
        let image = self.observation.take().map(|o| o.image);
        let stopped = matches!(self.status, "denied" | "stopped");
        let failed = stopped || self.uncertain;
        let continuation = if stopped {
            "Stop this browser task. Do not retry or route around a denial, cancellation or human takeover. Preserve the existing action receipts."
        } else {
            "Inspect current state before continuing with normal computer tools. Preserve these action receipts; never repeat a completed or uncertain action. A finish decision is not independent confirmation of remote success."
        };
        let mut result = json!({"text":json!({"browser_worker":self,
            "continuation":continuation}).to_string()});
        if let Some(image) = image {
            result["image"] = json!(image);
        }
        result["failed"] = json!(failed);
        if stopped {
            result["stopped"] = json!(true);
        }
        // Existing provider recovery must not automatically replay an uncertain write.
        if self.uncertain {
            result["timed_out"] = json!(true);
        }
        result
    }
}

async fn drive<D: Decisions + ?Sized, B: Browser, G: Guard>(
    selector: &D,
    browser: &mut B,
    guard: &G,
    task: &Task,
) -> Outcome {
    drive_until(
        selector,
        browser,
        guard,
        task,
        tokio::time::Instant::now() + Duration::from_secs(120),
    )
    .await
}
async fn drive_until<D: Decisions + ?Sized, B: Browser, G: Guard>(
    selector: &D,
    browser: &mut B,
    guard: &G,
    task: &Task,
    deadline: tokio::time::Instant,
) -> Outcome {
    let mut outcome = Outcome::new("fallback", "Browser worker action limit reached");
    let mut applied_clicks = std::collections::HashSet::new();
    // One final observation/decision is allowed after the last permitted action.
    for step in 0..=task.max_actions {
        if let Err(error) = guard.check() {
            outcome.status = "stopped";
            outcome.reason = error.to_string();
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            outcome.reason = "Browser worker time limit reached; inspect current state".into();
            break;
        }
        let observation = match tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(30)),
            browser.observe(task),
        )
        .await
        .unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                "Browser observation timed out; no action attempted"
            ))
        })
        .and_then(|o| {
            o.validate(task)?;
            Ok(o)
        }) {
            Ok(observation) => observation,
            Err(error) => {
                outcome.reason = error.to_string();
                break;
            }
        };
        outcome.observation = Some(observation.clone());
        let input = DecisionInput {
            goal: task.goal.clone(),
            values: task.values.clone(),
            observation,
            previous_actions: outcome.actions.clone(),
        };
        let choice = match tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(15)),
            selector.choose(&input),
        )
        .await
        {
            Ok(Ok(choice)) => choice,
            Ok(Err(error)) => {
                outcome.reason = format!(
                    "Decisions unavailable: {error}. Continue from the observed state; no new input was dispatched"
                );
                break;
            }
            Err(_) => {
                outcome.reason =
                    "Decisions timed out; continue from the observed state without replay".into();
                break;
            }
        };
        if let Err(error) = guard.check() {
            outcome.status = "stopped";
            outcome.reason = error.to_string();
            break;
        }
        let Some(candidate) = input.observation.candidates.iter().find(|c| c.id == choice) else {
            outcome.reason = "Decisions returned a choice outside the observed action set".into();
            break;
        };
        if candidate.terminal() {
            outcome.status = if candidate.kind == "finish" {
                "reported_finished"
            } else {
                "fallback"
            };
            outcome.reason = if candidate.kind == "finish" {
                "The selector reports the goal satisfied; verify the final observed result. No independent success confirmation was made"
            } else {
                "Browser needs Codex planning or human assistance"
            }
            .into();
            break;
        }
        // A successful CDP click is dispatch confirmation, not proof a delayed
        // submission settled. Never repeat it on an unchanged observed page.
        let click_key = (
            input.observation.url.clone(),
            input.observation.text.clone(),
            candidate.label.clone(),
        );
        if candidate.kind == "click" && applied_clicks.contains(&click_key) {
            outcome.reason = "This click was already dispatched on the same observed state. Its remote result is not confirmed; inspect current state without replay".into();
            outcome.uncertain = true;
            break;
        }
        if step == task.max_actions {
            break;
        }
        match tokio::time::timeout_at(deadline, guard.authorize(task, candidate))
            .await
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "Browser task timed out before input authorization"
                ))
            }) {
            Ok(true) => {}
            Ok(false) => {
                outcome.status = "denied";
                outcome.reason =
                    "The user declined this action. Do not retry or route around this decision."
                        .into();
                break;
            }
            Err(error) => {
                outcome.status = "stopped";
                outcome.reason = error.to_string();
                break;
            }
        }
        if let Err(error) = guard.check() {
            outcome.status = "stopped";
            outcome.reason = error.to_string();
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            outcome.reason = "Browser worker time limit reached before input".into();
            break;
        }
        let id = match guard.requested(candidate) {
            Ok(id) => id,
            Err(error) => {
                outcome.reason = error.to_string();
                break;
            }
        };
        // After dispatch a broken connection is an uncertain effect, never an
        // invitation to repeat the action or switch to the legacy executor.
        let receipt = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(30)),
            browser.act(&input.observation, candidate),
        )
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("Browser input timed out")))
        .unwrap_or(ActionReceipt {
            applied: false,
            uncertain: true,
            detail: "Browser action response lost; inspect current state before continuing".into(),
        });
        if candidate.kind == "click" && receipt.applied {
            applied_clicks.insert(click_key);
        }
        outcome.uncertain |= receipt.uncertain;
        outcome.actions.push(
            json!({"call_id":id,"choice_id":candidate.id,"label":candidate.label,
            "applied":receipt.applied,"uncertain":receipt.uncertain,"detail":receipt.detail}),
        );
        // The screenshot preceding a dispatched action is no longer current.
        outcome.observation = None;
        if let Err(error) = guard.receipt(&id, candidate, &receipt) {
            outcome.uncertain = true;
            outcome.reason = error.to_string();
            break;
        }
        if !receipt.applied || receipt.uncertain {
            outcome.reason = receipt.detail;
            break;
        }
    }
    outcome
}

struct RuntimeGuard<'a> {
    app: &'a App,
    bot: &'a Bot,
    run: &'a Run,
    credential: Option<&'a std::sync::Mutex<Option<Vec<u8>>>>,
}
impl Guard for RuntimeGuard<'_> {
    fn check(&self) -> Result<()> {
        if let Some(credential) = self.credential {
            if let Some(expected) = credential.lock().unwrap().as_ref() {
                let current = crate::connections::decisions_key(self.app).map(|key| {
                    ring::digest::digest(&ring::digest::SHA256, key.as_bytes())
                        .as_ref()
                        .to_vec()
                });
                ensure!(
                    current.as_ref() == Some(expected),
                    "Decisions key changed or was removed; no further browser input authorized"
                );
            }
        }
        ensure!(
            self.run.bot_id == self.bot.id,
            "Browser run belongs to another bot"
        );
        ensure!(
            !self.app.account_disabled() && !self.app.db.cancelled(&self.run.id),
            "Run cancelled"
        );
        ensure!(
            self.app.db.bot(&self.bot.id)?.provider == "codex",
            "Bot provider changed"
        );
        ensure!(
            !self.app.db.turn_deferred(&self.run.id)?,
            "Task is waiting for user input"
        );
        let slot = self.app.db.screen(&self.bot.id)?;
        ensure!(
            !self.app.db.screen_takeover(slot)?,
            "The user controls this desktop"
        );
        crate::vm_maintenance::available(&self.app.db)?;
        Ok(())
    }
    fn authorize<'a>(&'a self, task: &'a Task, candidate: &'a Candidate) -> Work<'a, bool> {
        Box::pin(async move {
            crate::runtime::approve(self.app, self.bot, self.run, "computer_browser_task",
                &json!({"goal":task.goal,"origin":task.origin,"action":candidate,"value":candidate.value_key.as_ref().and_then(|key|task.values.get(key)),"action_scope":candidate.scope()})).await
        })
    }
    fn requested(&self, candidate: &Candidate) -> Result<String> {
        crate::provider_retry::check_tool_budget(self.app, self.run)?;
        let id = db::id();
        let body = json!({"tool":"computer_browser_task","args":{"action":candidate},"call_id":id});
        self.app
            .db
            .event(&self.run.id, "tool_requested", body.clone())?;
        self.app.db.event(&self.run.id, "tool_started", body)?;
        Ok(id)
    }
    fn receipt(&self, id: &str, candidate: &Candidate, receipt: &ActionReceipt) -> Result<()> {
        self.app.db.event(&self.run.id, "tool_result", json!({"tool":"computer_browser_task","call_id":id,
            "choice_id":candidate.id,"failed":!receipt.applied,"timed_out":receipt.uncertain,"text":receipt.detail}))?;
        Ok(())
    }
}

pub fn preferred(app: &App, bot: &Bot) -> bool {
    bot.provider == "codex"
        && (app.decisions.is_some() || crate::connections::decisions_key(app).is_some())
}
pub fn prepare_open(app: &App, bot: &Bot, args: &mut Value) {
    // This flag is server-owned and never part of a model's tool schema.
    if let Some(object) = args.as_object_mut() {
        object.remove("_decisions_observation");
        if preferred(app, bot) {
            object.insert("_decisions_observation".into(), json!(true));
        }
    }
}
pub fn spec() -> Value {
    json!({"type":"function","name":"computer_browser_task",
        "description":"Preferred browser worker for Codex bots with an explicitly configured OpenAI Decisions API-key connection. Uses your existing VM browser profile and focused tab; first open the site with normal computer tools. Supply an exact origin, goal and named exact form values. Selects grounded page actions, uses Kindred approvals and verifies fields. Never supply secrets. On fallback inspect current state and preserve completed/uncertain receipts with ordinary computer tools; never replay them or bypass denial. A finish selection is not independent proof of remote success.",
        "inputSchema":{"type":"object","properties":{
            "goal":{"type":"string","maxLength":8000},
            "origin":{"type":"string","description":"Exact HTTP(S) origin without a path or credentials"},
            "values":{"type":"object","additionalProperties":{"type":"string","maxLength":16000},"maxProperties":16},
            "max_actions":{"type":"integer","minimum":1,"maximum":64}},
            "required":["goal","origin"],"additionalProperties":false}})
}
pub fn instructions(app: &App, bot: &Bot) -> &'static str {
    if bot.provider != "codex" {
        return "";
    }
    if preferred(app, bot) {
        "\nBrowser execution: prefer computer_browser_task on your existing Bot Computer for browser work. Supply the exact origin, goal and named values. On fallback, continue from current observations and completed action receipts with normal computer tools; never replay completed or uncertain actions or bypass a denial.\n"
    } else {
        "\nBrowser execution: Decisions is preferred when a supported connection is available. This runtime has no configured OpenAI Decisions API-key connection; use your existing computer tools. Subscription OAuth and ordinary Luna access do not establish Decisions API access.\n"
    }
}
pub async fn call(app: &App, bot: &Bot, run: &Run, args: Value) -> Result<Value> {
    ensure!(
        bot.provider == "codex" && app.db.bot(&bot.id)?.provider == "codex",
        "This browser worker is only available to Codex bots"
    );
    let task: Task = serde_json::from_value(args)?;
    task.validate()?;
    if !preferred(app, bot) {
        return Ok(Outcome::new("unavailable", "OpenAI Decisions is not configured. Save an API key under Settings → Connections → Codex → Browser helper; Kindred supports the documented API-key route and does not use subscription OAuth for Decisions. Use the existing computer tools; no browser action was attempted.").result());
    }
    let configured = ConfiguredDecisions {
        app,
        last_key: Default::default(),
    };
    let selector: &dyn Decisions = app.decisions.as_deref().unwrap_or(&configured);
    let guard = RuntimeGuard {
        app,
        bot,
        run,
        credential: app.decisions.is_none().then_some(&configured.last_key),
    };
    guard.check()?;
    // Preserve the existing post-handoff screenshot requirement. Browser page
    // screenshots do not replace observation of the whole desktop/login flow.
    ensure!(
        !app.db.handoff_needs_observation(&run.id)?,
        "Take a fresh computer_screenshot after the person returns control before browser work"
    );
    // Keep the same lease, profile and pending-RPC interruption safeguards used
    // by the existing VM computer tools. Never launch or restart a browser here.
    let mut session = crate::desktop_sessions::enter(app, run, "computer_browser_task").await?;
    let mut browser = match driver::GuestBrowser::connect(&app.config.vm, app.db.screen(&bot.id)?)
        .await
    {
        Ok(browser) => browser,
        Err(_) => {
            drop(session);
            app.desktop_sessions.release(&run.id);
            return Ok(Outcome::new("unavailable", "Existing browser has no supported observation connection. Continue with ordinary computer tools; no action attempted.").result());
        }
    };
    let outcome = session
        .rpc(async { Ok(drive(selector, &mut browser, &guard, &task).await.result()) })
        .await;
    drop(browser);
    drop(session);
    app.desktop_sessions.release(&run.id);
    outcome
}

struct ConfiguredDecisions<'a> {
    app: &'a App,
    last_key: std::sync::Mutex<Option<Vec<u8>>>,
}
impl Decisions for ConfiguredDecisions<'_> {
    fn choose<'a>(&'a self, input: &'a DecisionInput) -> Work<'a, String> {
        Box::pin(async move {
            let key = crate::connections::decisions_key(self.app).ok_or_else(|| {
                anyhow::anyhow!("Decisions key was removed or is unavailable; no action selected")
            })?;
            *self.last_key.lock().unwrap() = Some(
                ring::digest::digest(&ring::digest::SHA256, key.as_bytes())
                    .as_ref()
                    .to_vec(),
            );
            let result = transport::OpenAiDecisions::new(key)?.choose(input).await;
            let _=self.app.db.save_setting("decisions_last_use",&json!({"fingerprint":self.last_key.lock().unwrap().clone(),"error":result.as_ref().err().map(|e|e.to_string())}));
            result
        })
    }
}
