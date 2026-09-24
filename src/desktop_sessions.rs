//! Reserve a display only during a desktop interaction, never for an entire model turn.
use crate::{
    db::{self, Run},
    runtime::App,
};
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Entry>>);
struct Entry {
    state: Arc<AsyncMutex<Session>>,
    engaged: Arc<AtomicBool>,
}
impl Default for Entry {
    fn default() -> Self {
        let engaged = Arc::new(AtomicBool::new(false));
        Self {
            state: Arc::new(AsyncMutex::new(Session {
                engaged: engaged.clone(),
                ..Default::default()
            })),
            engaged,
        }
    }
}
#[derive(Default)]
pub struct Session {
    lease: Option<OwnedMutexGuard<()>>,
    observed: bool,
    engaged: Arc<AtomicBool>,
}
impl Sessions {
    fn session(&self, run: &str) -> Arc<AsyncMutex<Session>> {
        self.0
            .lock()
            .unwrap()
            .entry(run.into())
            .or_default()
            .state
            .clone()
    }
    pub fn engaged(&self, run: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(run)
            .is_some_and(|s| s.engaged.load(Ordering::SeqCst))
    }
    pub fn release(&self, run: &str) {
        if let Some(s) = self.0.lock().unwrap().get(run) {
            if let Ok(mut state) = s.state.try_lock() {
                state.lease.take();
                state.observed = false;
                s.engaged.store(false, Ordering::SeqCst);
            }
        }
    }
    pub fn remove(&self, run: &str) {
        self.0.lock().unwrap().remove(run);
    }
}
pub struct Cleanup<'a> {
    pub app: &'a App,
    pub run: &'a str,
    pub slot: i64,
    pub clean: bool,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if !self.clean && self.app.desktop_sessions.engaged(self.run) {
            // An interrupted guest RPC may outlive its SSH connection. Mark the
            // display unavailable before dropping ownership, including task abort.
            let _ = self.app.db.screen_set_quiet(self.slot, db::now() + 65);
        }
        self.app.desktop_sessions.remove(self.run);
    }
}

pub fn uses_desktop(tool: &str, args: &serde_json::Value) -> bool {
    matches!(
        tool,
        "computer_screenshot"
            | "computer_open_url"
            | "computer_click"
            | "computer_type"
            | "computer_key"
            | "computer_scroll"
    ) || (tool == "guest_exec" && args["use_desktop"] == true)
}
pub async fn enter(app: &App, run: &Run, tool: &str) -> Result<OwnedMutexGuard<Session>> {
    let mut session = app.desktop_sessions.session(&run.id).lock_owned().await;
    let slot = app.db.screen(&run.bot_id)?;
    ensure!(!app.db.cancelled(&run.id), "Run cancelled");
    if session.lease.is_none() {
        ensure!(
            !matches!(
                tool,
                "computer_click" | "computer_type" | "computer_key" | "computer_scroll"
            ),
            "Desktop control was released. Take a fresh computer_screenshot before acting; the screen may have changed."
        );
        loop {
            ensure!(
                !app.db.cancelled(&run.id) && !app.account_disabled(),
                "Run cancelled"
            );
            crate::vm_maintenance::available(&app.db)?;
            ensure!(
                !app.db.screen_takeover(slot)?,
                "The user controls this desktop. Continue non-desktop work or wait for them to return control."
            );
            if app.db.screen_quiet(slot)? <= db::now() {
                if let Ok(lease) = app.screen_lock(slot).try_lock_owned() {
                    ensure!(
                        !app.db.screen_takeover(slot)?,
                        "The user controls this desktop"
                    );
                    session.lease = Some(lease);
                    session.engaged.store(true, Ordering::SeqCst);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    if matches!(
        tool,
        "computer_click" | "computer_type" | "computer_key" | "computer_scroll"
    ) {
        ensure!(
            session.observed,
            "Take a fresh computer_screenshot before acting on this desktop"
        );
    }
    Ok(session)
}
impl Session {
    pub fn observed(&mut self, result: &serde_json::Value) {
        self.observed =
            result["failed"] != true && result["image"].as_str().is_some_and(|s| !s.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn run(app: &App) -> (crate::db::Bot, Run, i64) {
        let bot = crate::tests::bot(&app.db, "codex");
        app.db.queue(&bot.id, "Review the report", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let slot = app.db.screen(&bot.id).unwrap();
        (bot, run, slot)
    }
    #[tokio::test]
    async fn desktop_sequences_are_exclusive_and_release_requires_new_observation() {
        let app = crate::tests::app();
        let (bot, run, slot) = run(&app);
        {
            let mut session = enter(&app, &run, "computer_screenshot").await.unwrap();
            session.observed(&json!({"image":"fixture"}));
        }
        assert!(app.screen_lock(slot).try_lock_owned().is_err());
        assert!(enter(&app, &run, "computer_click").await.is_ok());
        let result = crate::runtime::call_tool(&app, &bot, &run, "computer_release", json!({}))
            .await
            .unwrap();
        assert_ne!(result["failed"], true);
        assert!(app.screen_lock(slot).try_lock_owned().is_ok());
        assert!(enter(&app, &run, "computer_click").await.is_err());
        let rejected = tokio::time::timeout(
            Duration::from_secs(1),
            crate::runtime::call_tool(
                &app,
                &bot,
                &run,
                "computer_click",
                json!({"x":1,"y":2,"action_scope":"external"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(rejected["failed"], true);
        assert!(
            app.db.approvals().unwrap().is_empty(),
            "Do not ask approval for a stale-screen action"
        );
        let mut session = enter(&app, &run, "computer_screenshot").await.unwrap();
        session.observed(&json!({"failed":true}));
        drop(session);
        assert!(enter(&app, &run, "computer_type").await.is_err());
        app.desktop_sessions.release(&run.id);
        crate::screen_control::set(&app.db, slot, true, "manual").unwrap();
        assert!(enter(&app, &run, "computer_screenshot").await.is_err());
        assert!(!app.desktop_sessions.engaged(&run.id));
    }
    #[tokio::test]
    async fn model_and_context_work_continue_concurrently_while_desktops_are_locked() {
        let app = crate::tests::app();
        let (a, one, slot) = run(&app);
        let (_, two, other) = run(&app);
        let _human = app.screen_lock(slot).lock_owned().await;
        let _other = app.screen_lock(other).lock_owned().await;
        crate::screen_control::set(&app.db, slot, true, "manual").unwrap();
        let barrier = tokio::sync::Barrier::new(2);
        let mut first = None;
        let mut second = None;
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                crate::runtime::drive_run(&app, &one, slot, &mut first, async {
                    barrier.wait().await;
                    let result =
                        crate::runtime::call_tool(&app, &a, &one, "bots_list", json!({})).await?;
                    ensure!(result["failed"] != true, "Context read failed");
                    Ok("one".into())
                }),
                crate::runtime::drive_run(&app, &two, other, &mut second, async {
                    barrier.wait().await;
                    Ok("two".into())
                })
            )
        })
        .await
        .unwrap();
        assert_eq!(result.0.unwrap().0.unwrap(), "one");
        assert_eq!(result.1.unwrap().0.unwrap(), "two");
    }
    #[tokio::test]
    async fn completed_run_releases_lazy_desktop_lease_and_other_tools_release_it() {
        let app = crate::tests::app();
        let (bot, run, slot) = run(&app);
        let mut lease = None;
        let (output, abrupt) = crate::runtime::drive_run(&app, &run, slot, &mut lease, async {
            drop(enter(&app, &run, "computer_screenshot").await?);
            crate::runtime::call_tool(&app, &bot, &run, "bots_list", json!({})).await?;
            ensure!(
                app.screen_lock(slot).try_lock_owned().is_ok(),
                "Read-only tool retained desktop"
            );
            drop(enter(&app, &run, "computer_screenshot").await?);
            Ok("done".into())
        })
        .await
        .unwrap();
        assert!(output.is_ok());
        assert!(!abrupt);
        assert!(app.screen_lock(slot).try_lock_owned().is_ok());
    }
}

#[cfg(test)]
mod interruption_tests {
    use super::*;
    #[tokio::test]
    async fn cancelling_desktop_work_recovers_the_screen_but_chat_cancellation_does_not_pause_it() {
        for desktop in [false, true] {
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, "codex");
            app.db.queue(&bot.id, "Work", 0).unwrap();
            let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
            let slot = app.db.screen(&bot.id).unwrap();
            let mut lease = None;
            let (result, abrupt) = crate::runtime::drive_run(&app, &run, slot, &mut lease, async {
                if desktop {
                    drop(enter(&app, &run, "computer_screenshot").await?);
                }
                app.db.cancel(&run.id)?;
                std::future::pending::<()>().await;
                Ok(String::new())
            })
            .await
            .unwrap();
            assert!(result.is_err());
            assert_eq!(abrupt, desktop);
            assert_eq!(app.db.screen_quiet(slot).unwrap() > db::now(), desktop);
            assert!(app.screen_lock(slot).try_lock_owned().is_ok());
        }
    }
    #[tokio::test]
    async fn waiting_for_desktop_control_can_be_cancelled_without_acquiring_it() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "codex");
        app.db.queue(&bot.id, "Work", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let slot = app.db.screen(&bot.id).unwrap();
        let _held = app.screen_lock(slot).lock_owned().await;
        let wait = enter(&app, &run, "computer_screenshot");
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(120)).await;
            app.db.cancel(&run.id).unwrap();
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(wait, cancel) })
                .await
                .unwrap();
        assert!(result.is_err());
        assert!(!app.desktop_sessions.engaged(&run.id));
    }
}
