// Long-task runtime policy. A task stops when it makes no recorded progress
// for the idle window, or when an optional active-time cap is reached. Time
// spent waiting for a person or an approval is never charged, and a tool that
// is still running receives a longer inactivity window as a backstop.
use crate::{config::Config, db::Db};
use anyhow::Result;
use std::collections::BTreeMap;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

/// How often the watchdog reads activity from the database.
pub const POLL: Duration = if cfg!(test) { Duration::from_millis(50) } else { Duration::from_secs(1) };

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub idle: Duration,
    pub total: Option<Duration>,
}
impl Limits {
    pub fn from_config(config: &Config) -> Self {
        let total = config.task_timeout_seconds();
        Self {
            idle: Duration::from_secs(config.idle_timeout_seconds),
            total: (total > 0).then(|| Duration::from_secs(total)),
        }
    }
}

/// One cheap read of the run's durable state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    cursor: i64,
    calls: BTreeMap<String, String>,
    /// Latest genuine work event: assistant text, reasoning, tool request,
    /// start or result, context and handoff records.
    pub work_seq: i64,
    /// Latest model progress state. Repeating the same state is a keepalive.
    pub progress: Option<(i64, String)>,
    /// A person or an approval decision is holding the run.
    pub waiting: bool,
    /// A tool request without a matching result.
    pub outstanding_tool: Option<String>,
}

pub fn snapshot(db: &Db, run: &str) -> Result<Snapshot> {
    snapshot_since(db, run, Snapshot::default())
}

// Only consume new events. Rescanning every request/result pair once a second
// would make the watchdog increasingly expensive during an overnight task.
fn snapshot_since(db: &Db, run: &str, mut previous: Snapshot) -> Result<Snapshot> {
    let c = db.0.lock().unwrap();
    let mut statement = c.prepare("SELECT seq,kind,body FROM events WHERE run_id=?1 AND seq>?2 ORDER BY seq")?;
    let rows = statement.query_map(rusqlite::params![run, previous.cursor], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, String>(1)?,r.get::<_, String>(2)?)))?;
    for row in rows {
        let (seq, kind, raw) = row?;
        previous.cursor = seq;
        if !matches!(kind.as_str(), "assistant"|"reasoning"|"tool_requested"|"tool_started"|"tool_result"|"context"|"context_selection"|"context_assembly"|"runtime_context"|"handoff"|"user_action_done"|"approval"|"question_wait"|"model_progress") { continue; }
        let body: Value = serde_json::from_str(&raw)?;
        if kind == "model_progress" {
            previous.progress = Some((seq, body["state"].as_str().unwrap_or("").into()));
        } else if kind != "assistant" || body["text"].as_str().is_some_and(|s| !s.trim().is_empty()) {
            previous.work_seq = seq;
        }
        if !body["call_id"].is_null() {
            let id = body["call_id"].to_string();
            if kind == "tool_requested" {
                previous.calls.insert(id, body["tool"].as_str().unwrap_or("tool").into());
            } else if kind == "tool_result" {
                previous.calls.remove(&id);
            }
        }
    }
    previous.waiting = c.query_row("SELECT EXISTS(SELECT 1 FROM user_tasks WHERE run_id=?1 AND status IN ('pending','ready')) OR EXISTS(SELECT 1 FROM approvals WHERE run_id=?1 AND status='pending') OR EXISTS(SELECT 1 FROM runs WHERE id=?1 AND status IN ('awaiting_user','awaiting_approval'))", [run], |r| r.get(0))?;
    previous.outstanding_tool = previous.calls.values().next().cloned();
    Ok(previous)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reason {
    Idle,
    Total,
}

/// Typed, nonretryable stop. The run finishes as interrupted so the existing
/// Continue action can pick it up; nothing is queued or replayed automatically.
#[derive(Debug)]
pub struct RunLimitReached {
    pub reason: Reason,
    pub active: Duration,
    pub idle: Duration,
    pub outstanding_tool: Option<String>,
}
impl RunLimitReached {
    pub fn event(&self, limits: &Limits) -> Value {
        json!({
            "reason": match self.reason { Reason::Idle => "idle", Reason::Total => "task_timeout" },
            "active_seconds": self.active.as_secs(),
            "idle_seconds": self.idle.as_secs(),
            "idle_timeout_seconds": limits.idle.as_secs(),
            "task_timeout_seconds": limits.total.map(|d| d.as_secs()),
            "tool_in_flight": self.outstanding_tool.is_some(),
            "outstanding_tool": self.outstanding_tool,
        })
    }
}
impl std::fmt::Display for RunLimitReached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let minutes = |d: Duration| d.as_secs().div_ceil(60).max(1);
        match self.reason {
            Reason::Idle => write!(f, "Paused after {} minutes without progress.", minutes(self.idle))?,
            Reason::Total => write!(f, "Reached this task's {}-minute time limit.", minutes(self.active))?,
        }
        if let Some(tool) = &self.outstanding_tool {
            write!(f, " {tool} had not reported a result.")?;
        }
        write!(f, " Completed actions are preserved; review Activity, then Continue.")
    }
}
impl std::error::Error for RunLimitReached {}

pub fn is_limit(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RunLimitReached>().is_some() || error.is::<crate::provider_retry::ActionLimit>()
}

pub struct Watch {
    limits: Limits,
    seen: Snapshot,
    last_poll: Instant,
    last_activity: Instant,
    active: Duration,
}
impl Watch {
    pub fn new(limits: Limits, now: Instant, initial: Snapshot) -> Self {
        Self { limits, seen: initial, last_poll: now, last_activity: now, active: Duration::ZERO }
    }
    pub fn read(&self, db: &Db, run: &str) -> Result<Snapshot> {
        snapshot_since(db, run, self.seen.clone())
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    pub fn due(&self, now: Instant) -> bool {
        now.duration_since(self.last_poll) >= POLL
    }
    /// Advance the policy clock to `now` given the latest durable state.
    pub fn observe(&mut self, now: Instant, snap: Snapshot) -> Option<RunLimitReached> {
        let step = now.duration_since(self.last_poll);
        self.last_poll = now;
        // Status labels alone are not evidence of work, even if they alternate.
        let progressed = snap.work_seq > self.seen.work_seq;
        let waiting = snap.waiting;
        let outstanding_tool = snap.outstanding_tool.clone();
        self.seen = snap;
        if waiting {
            // Neither clock runs while a person or approval holds the task.
            self.last_activity = now;
            return None;
        }
        self.active += step;
        if progressed {
            self.last_activity = now;
        }
        let idle = now.duration_since(self.last_activity);
        let reason = if self.limits.total.is_some_and(|total| self.active >= total) {
            Reason::Total
        } else if idle >= if outstanding_tool.is_some() { self.limits.idle.max(Duration::from_secs(3600)) } else { self.limits.idle } {
            Reason::Idle
        } else {
            return None;
        };
        Some(RunLimitReached { reason, active: self.active, idle, outstanding_tool })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MIN: Duration = Duration::from_secs(60);
    fn snap(work_seq: i64) -> Snapshot {
        Snapshot { work_seq, ..Default::default() }
    }
    fn limits(idle: u64, total: Option<u64>) -> Limits {
        Limits { idle: MIN * idle as u32, total: total.map(|t| MIN * t as u32) }
    }

    #[test]
    fn productive_work_continues_past_the_old_cutoff() {
        let start = Instant::now();
        let mut watch = Watch::new(limits(30, None), start, snap(0));
        for minute in 1..=600 {
            assert!(watch.observe(start + MIN * minute, snap(minute as i64)).is_none());
        }
    }
    #[test]
    fn silence_stops_after_the_idle_window() {
        let start = Instant::now();
        let mut watch = Watch::new(limits(30, None), start, snap(4));
        assert!(watch.observe(start + MIN * 29, snap(4)).is_none());
        let stop = watch.observe(start + MIN * 30, snap(4)).unwrap();
        assert_eq!(stop.reason, Reason::Idle);
        assert_eq!(stop.idle, MIN * 30);
        assert!(stop.to_string().contains("30 minutes without progress"));
        assert_eq!(stop.event(watch.limits())["tool_in_flight"], false);
    }
    #[test]
    fn repeated_progress_pings_are_not_progress() {
        let start = Instant::now();
        let ping = |seq| Snapshot { progress: Some((seq, "waiting".into())), ..snap(1) };
        let mut watch = Watch::new(limits(30, None), start, ping(1));
        for minute in 1..30 {
            assert!(watch.observe(start + MIN * minute, ping(minute as i64 + 1)).is_none());
        }
        assert_eq!(watch.observe(start + MIN * 30, ping(40)).unwrap().reason, Reason::Idle);
        let mut watch = Watch::new(limits(30, None), start, ping(1));
        let thinking = Snapshot { progress: Some((2, "thinking".into())), ..snap(1) };
        assert!(watch.observe(start + MIN * 20, thinking.clone()).is_none());
        assert_eq!(watch.observe(start + MIN * 30, thinking).unwrap().reason, Reason::Idle);
    }
    #[test]
    fn explicit_cap_counts_active_time_even_while_busy() {
        let start = Instant::now();
        let mut watch = Watch::new(limits(30, Some(120)), start, snap(0));
        for minute in 1..120 {
            assert!(watch.observe(start + MIN * minute, snap(minute as i64)).is_none());
        }
        let stop = watch.observe(start + MIN * 120, snap(200)).unwrap();
        assert_eq!(stop.reason, Reason::Total);
        assert_eq!(stop.event(watch.limits())["task_timeout_seconds"], 7200);
    }
    #[test]
    fn people_and_approvals_are_not_charged() {
        let start = Instant::now();
        let waiting = Snapshot { waiting: true, ..snap(1) };
        let mut watch = Watch::new(limits(30, Some(60)), start, snap(1));
        assert!(watch.observe(start + MIN * 10, snap(1)).is_none());
        for hour in 1..=48 {
            assert!(watch.observe(start + MIN * (10 + 60 * hour), waiting.clone()).is_none());
        }
        let back = start + MIN * (10 + 60 * 48);
        assert!(watch.observe(back + MIN * 29, snap(1)).is_none());
        assert_eq!(watch.observe(back + MIN * 30, snap(1)).unwrap().reason, Reason::Idle);
        let mut watch = Watch::new(limits(600, Some(60)), start, snap(1));
        watch.observe(start + MIN * 10, snap(1));
        watch.observe(start + MIN * 500, waiting);
        assert!(watch.observe(start + MIN * 549, snap(2)).is_none());
        assert_eq!(watch.observe(start + MIN * 550, snap(3)).unwrap().reason, Reason::Total);
    }
    #[test]
    fn running_tool_uses_its_own_bounds_but_not_past_the_cap() {
        let start = Instant::now();
        let busy = Snapshot { outstanding_tool: Some("guest_exec".into()), ..snap(1) };
        let mut watch = Watch::new(limits(30, Some(60)), start, snap(1));
        assert!(watch.observe(start + MIN * 59, busy.clone()).is_none());
        let stop = watch.observe(start + MIN * 60, busy).unwrap();
        assert_eq!(stop.reason, Reason::Total);
        assert_eq!(stop.event(watch.limits())["outstanding_tool"], "guest_exec");
        assert!(stop.to_string().contains("guest_exec had not reported"));
    }
    #[test]
    fn missing_tool_receipt_cannot_disable_inactivity_forever() {
        let start = Instant::now();
        let busy = Snapshot { outstanding_tool: Some("connector".into()), ..snap(1) };
        let mut watch = Watch::new(limits(30, None), start, busy.clone());
        assert!(watch.observe(start + MIN * 59, busy.clone()).is_none());
        let stop = watch.observe(start + MIN * 60, busy).unwrap();
        assert_eq!(stop.reason, Reason::Idle);
        assert!(stop.outstanding_tool.is_some());
    }
    #[test]
    fn legacy_config_maps_to_limits() {
        let mut c = Config::default();
        assert_eq!(Limits::from_config(&c), Limits { idle: MIN * 30, total: None });
        c.run_timeout_seconds = 1800;
        assert_eq!(Limits::from_config(&c).total, None);
        c.run_timeout_seconds = 3600;
        assert_eq!(Limits::from_config(&c).total, Some(MIN * 60));
        c.task_timeout_seconds = Some(0);
        assert_eq!(Limits::from_config(&c).total, None);
    }
    #[test]
    fn incremental_snapshot_keeps_open_calls_without_rereading_history() {
        let app = crate::tests::app();
        let db = &app.db;
        let bot = crate::tests::bot(&db, "openrouter");
        let id = db.queue(&bot.id, "Long task", 0).unwrap();
        db.event(&id, "tool_requested", json!({"call_id":"first","tool":"guest_exec"})).unwrap();
        let first = snapshot(&db, &id).unwrap();
        assert_eq!(first.outstanding_tool.as_deref(), Some("guest_exec"));
        // Old body bytes must not be decoded again on every watchdog poll.
        db.0.lock().unwrap().execute("UPDATE events SET body='not-json' WHERE run_id=? AND kind='tool_requested'", [&id]).unwrap();
        db.event(&id, "tool_requested", json!({"call_id":"second","tool":"connector_execute"})).unwrap();
        let second = snapshot_since(&db, &id, first).unwrap();
        assert_eq!(second.calls.len(), 2);
        db.event(&id, "tool_result", json!({"call_id":"first","text":"Done"})).unwrap();
        let third = snapshot_since(&db, &id, second).unwrap();
        assert_eq!(third.outstanding_tool.as_deref(), Some("connector_execute"));
        db.event(&id, "tool_result", json!({"call_id":"second","text":"Done"})).unwrap();
        let last = snapshot_since(&db, &id, third).unwrap();
        assert!(last.outstanding_tool.is_none());
        assert_eq!(snapshot_since(&db, &id, last.clone()).unwrap(), last);
    }
    #[test]
    fn snapshot_reads_waits_tools_and_ignores_empty_assistant_events() {
        let db = Db::open(":memory:").unwrap();
        let bot = crate::tests::bot(&db, "openrouter");
        db.queue(&bot.id, "Work", 0).unwrap();
        let run = db.claim_bot(&bot.id).unwrap().unwrap();
        assert_eq!(snapshot(&db, &run.id).unwrap(), Snapshot::default());
        db.event(&run.id, "assistant", json!({"text":"  "})).unwrap();
        db.event(&run.id, "provider_retry", json!({})).unwrap();
        assert_eq!(snapshot(&db, &run.id).unwrap().work_seq, 0);
        db.event(&run.id, "tool_requested", json!({"tool":"guest_exec","call_id":"a"})).unwrap();
        let busy = snapshot(&db, &run.id).unwrap();
        assert!(busy.work_seq > 0);
        assert_eq!(busy.outstanding_tool.as_deref(), Some("guest_exec"));
        db.event(&run.id, "tool_result", json!({"tool":"guest_exec","call_id":"a"})).unwrap();
        assert_eq!(snapshot(&db, &run.id).unwrap().outstanding_tool, None);
        db.request_approval(&run.id, "connector_execute", &json!({})).unwrap();
        assert!(snapshot(&db, &run.id).unwrap().waiting);
    }

    // Real drive_run with one-second limits; the test-only POLL keeps these short.
    mod drive {
        use super::super::*;
        use crate::{runtime, tests::{app, bot}};
        use std::sync::Arc;
        use crate::runtime::Shared;
        fn setup(edit: impl FnOnce(&mut Config)) -> (Shared, crate::db::Run, i64) {
            let mut app = app();
            edit(&mut Arc::get_mut(&mut app).unwrap().config);
            let bot = bot(&app.db, "openrouter");
            app.db.queue(&bot.id, "Long task", 0).unwrap();
            let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
            let slot = app.db.screen(&bot.id).unwrap();
            (app, run, slot)
        }
        async fn drive(app: &Shared, run: &crate::db::Run, slot: i64, work: impl std::future::Future<Output = Result<String>>) -> Result<String> {
            let (result, _) = runtime::drive_run(app, run, slot, &mut None, work).await.unwrap();
            if let Err(error) = &result {
                app.db.finish(&run.id, runtime::failure_status(app, run, error), "", &error.to_string()).unwrap();
            }
            result
        }
        fn limit_events(app: &Shared, run: &str) -> Vec<Value> {
            app.db.events(run).unwrap().into_iter().filter(|e| e["kind"] == "run_limit").collect()
        }

        #[tokio::test]
        async fn productive_work_outlasts_the_idle_window() {
            let (app, run, slot) = setup(|c| { c.idle_timeout_seconds = 1; c.run_timeout_seconds = 1800; });
            let result = drive(&app, &run, slot, async {
                for step in 0..12 {
                    app.db.event(&run.id, "assistant", json!({"text":format!("step {step}")}))?;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok("done".into())
            }).await;
            assert_eq!(result.unwrap(), "done");
            assert!(limit_events(&app, &run.id).is_empty());
        }
        #[tokio::test]
        async fn silence_interrupts_with_a_receipt_and_no_replay() {
            let (app, run, slot) = setup(|c| c.idle_timeout_seconds = 1);
            let started = Instant::now();
            let error = drive(&app, &run, slot, async {
                loop {
                    app.db.event(&run.id, "model_progress", json!({"state":"waiting"}))?;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }).await.unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(3));
            assert_eq!(error.downcast_ref::<RunLimitReached>().unwrap().reason, Reason::Idle);
            let receipt = limit_events(&app, &run.id);
            assert_eq!(receipt.len(), 1);
            assert_eq!(receipt[0]["body"]["reason"], "idle");
            assert_eq!(receipt[0]["body"]["tool_in_flight"], false);
            let finished = app.db.run(&run.id).unwrap();
            assert_eq!(finished.status, "interrupted");
            assert!(finished.error.contains("Continue"));
            assert!(app.db.claim_bot(&run.bot_id).unwrap().is_none(), "no automatic follow-up run");
        }
        #[tokio::test]
        async fn explicit_cap_stops_busy_work() {
            let (app, run, slot) = setup(|c| { c.idle_timeout_seconds = 3600; c.task_timeout_seconds = Some(1); });
            let error = drive(&app, &run, slot, async {
                for step in 0.. {
                    app.db.event(&run.id, "assistant", json!({"text":format!("step {step}")}))?;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                unreachable!()
            }).await.unwrap_err();
            assert!(runtime::failure_status(&app, &run, &error) == "interrupted");
            assert_eq!(limit_events(&app, &run.id)[0]["body"]["reason"], "task_timeout");
        }
        #[tokio::test]
        async fn approval_wait_and_running_tool_are_not_idle() {
            let (app, run, slot) = setup(|c| { c.idle_timeout_seconds = 1; c.task_timeout_seconds = Some(2); });
            let result = drive(&app, &run, slot, async {
                let approval = app.db.request_approval(&run.id, "connector_execute", &json!({}))?;
                tokio::time::sleep(Duration::from_millis(2500)).await;
                app.db.decide(&approval, true)?;
                app.db.event(&run.id, "tool_requested", json!({"tool":"guest_exec","call_id":"slow"}))?;
                tokio::time::sleep(Duration::from_millis(1500)).await;
                app.db.event(&run.id, "tool_result", json!({"tool":"guest_exec","call_id":"slow"}))?;
                Ok("approved and finished".into())
            }).await;
            assert_eq!(result.unwrap(), "approved and finished");
            assert!(limit_events(&app, &run.id).is_empty());
        }
        #[tokio::test]
        async fn stop_is_still_immediate() {
            let (app, run, slot) = setup(|c| c.idle_timeout_seconds = 3600);
            let stopper = app.clone();
            let id = run.id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                stopper.db.cancel(&id).unwrap();
            });
            let started = Instant::now();
            let error = drive(&app, &run, slot, std::future::pending()).await.unwrap_err();
            assert!(started.elapsed() < Duration::from_millis(800));
            assert!(error.to_string().contains("Stopped by the user"));
            assert_eq!(app.db.run(&run.id).unwrap().status, "cancelled");
            assert!(limit_events(&app, &run.id).is_empty());
        }
    }
}
