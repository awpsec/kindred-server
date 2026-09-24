//! A human subtask suspends the existing provider call and releases only its screen lease.
use crate::{
    db::{self, Db, Run},
    runtime::App,
};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize)]
pub struct UserTask {
    pub id: String,
    pub run_id: String,
    pub bot_id: String,
    pub title: String,
    pub instructions: String,
    pub status: String,
    pub outcome: String,
    pub created: i64,
    pub authentication: Option<Value>,
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UserTask> {
    Ok(UserTask {
        id: r.get(0)?,
        run_id: r.get(1)?,
        bot_id: r.get(2)?,
        title: r.get(3)?,
        instructions: r.get(4)?,
        status: r.get(5)?,
        outcome: r.get(6)?,
        created: r.get(7)?,
        authentication: r.get::<_, Option<String>>(8)?.and_then(|s| serde_json::from_str(&s).ok()),
    })
}
pub fn migrate(c: &rusqlite::Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS user_tasks(id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),bot_id TEXT NOT NULL REFERENCES bots(id),title TEXT NOT NULL,instructions TEXT NOT NULL,status TEXT NOT NULL,outcome TEXT NOT NULL DEFAULT '',created INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS user_task_auth(task_id TEXT PRIMARY KEY REFERENCES user_tasks(id) ON DELETE CASCADE,metadata TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS user_task_code_attempt(task_id TEXT PRIMARY KEY REFERENCES user_tasks(id) ON DELETE CASCADE);
        CREATE UNIQUE INDEX IF NOT EXISTS user_tasks_pending ON user_tasks(run_id) WHERE status IN ('pending','ready');
        UPDATE user_tasks SET status='expired' WHERE status IN ('pending','ready');")?;
    Ok(())
}
impl Db {
    pub fn handoff_needs_observation(&self, run: &str) -> Result<bool> {
        let (done,seen):(Option<i64>,Option<i64>)=self.0.lock().unwrap().query_row(
            "SELECT MAX(CASE WHEN kind='user_action_done' THEN seq END),MAX(CASE WHEN kind='tool_result' AND json_extract(body,'$.tool')='computer_screenshot' AND json_extract(body,'$.has_image')=1 AND COALESCE(json_extract(body,'$.failed'),0)=0 THEN seq END) FROM events WHERE run_id=?",[run],|r|Ok((r.get(0)?,r.get(1)?)))?;
        Ok(done.is_some_and(|done| seen.is_none_or(|seen| seen < done)))
    }
    // A click receipt is not evidence of the resulting page. Require a fresh
    // observation after this run's latest computer action before a handoff.
    pub fn require_observed_handoff(&self, run: &str) -> Result<()> {
        let (action,observed):(Option<i64>,Option<i64>)=self.0.lock().unwrap().query_row(
            "SELECT MAX(CASE WHEN json_extract(body,'$.tool') IN ('computer_open_url','computer_click','computer_type','computer_key','computer_scroll') AND COALESCE(json_extract(body,'$.failed'),0)=0 THEN seq END),MAX(CASE WHEN json_extract(body,'$.tool')='computer_screenshot' AND json_extract(body,'$.has_image')=1 AND COALESCE(json_extract(body,'$.failed'),0)=0 THEN seq END) FROM events WHERE run_id=? AND kind='tool_result'",[run],|r|Ok((r.get(0)?,r.get(1)?)))?;
        ensure!(
            action.is_none_or(|action| observed.is_some_and(|seen| seen > action)),
            "Inspect a fresh computer_screenshot after your last computer action before requesting user help. Describe the actual visible page; a successful click does not prove the expected form or verification step appeared."
        );
        Ok(())
    }
    pub fn user_tasks(&self) -> Result<Vec<UserTask>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .prepare("SELECT user_tasks.*,(SELECT metadata FROM user_task_auth WHERE task_id=user_tasks.id) FROM user_tasks ORDER BY CASE WHEN status IN ('pending','ready') THEN 0 ELSE 1 END,created DESC,rowid DESC LIMIT 500")?
            .query_map([], row)?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn user_task(&self, id: &str) -> Result<UserTask> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT user_tasks.*,(SELECT metadata FROM user_task_auth WHERE task_id=user_tasks.id) FROM user_tasks WHERE id=?", [id], row)?)
    }
    pub fn pending_user_task(&self, run: &str) -> Result<Option<UserTask>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT user_tasks.*,(SELECT metadata FROM user_task_auth WHERE task_id=user_tasks.id) FROM user_tasks WHERE run_id=? AND status IN ('pending','ready')",
                [run],
                row,
            )
            .optional()?)
    }
    pub fn request_user_task(
        &self,
        run: &Run,
        title: &str,
        instructions: &str,
    ) -> Result<UserTask> {
        self.request_user_task_with_auth(run, title, instructions, None)
    }
    pub fn request_user_task_with_auth(&self, run: &Run, title: &str, instructions: &str, authentication: Option<Value>) -> Result<UserTask> {
        if let Some(ref auth) = authentication { validate_authentication(auth)?; }
        let (title, instructions) = (title.trim(), instructions.trim());
        ensure!(
            !title.is_empty()
                && title.chars().count() <= 120
                && !title.chars().any(char::is_control),
            "Use a short subtask title"
        );
        ensure!(
            !instructions.is_empty() && instructions.len() <= 4000,
            "Describe what you need the user to do"
        );
        let id = db::id();
        let task = UserTask {
            id,
            run_id: run.id.clone(),
            bot_id: run.bot_id.clone(),
            title: title.into(),
            instructions: instructions.into(),
            status: "pending".into(),
            outcome: String::new(),
            created: db::now(),
            authentication,
        };
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        ensure!(tx.execute("UPDATE runs SET status='awaiting_user' WHERE id=? AND bot_id=? AND status='running'", params![run.id,run.bot_id])? == 1, "This task is no longer running");
        tx.execute(
            "INSERT INTO user_tasks VALUES(?,?,?,?,?,?,?,?)",
            params![
                task.id,
                task.run_id,
                task.bot_id,
                task.title,
                task.instructions,
                task.status,
                task.outcome,
                task.created
            ],
        )?;
        if let Some(ref auth) = task.authentication {
            tx.execute("INSERT INTO user_task_auth VALUES(?,?)", params![task.id, serde_json::to_string(auth)?])?;
        }
        tx.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,'user_action',?,?)",
            params![run.id, serde_json::to_string(&task)?, task.created],
        )?;
        tx.commit()?;
        Ok(task)
    }
    pub fn reserve_code_entry(&self, task: &UserTask) -> Result<()> {
        let c = self.0.lock().unwrap();
        ensure!(c.execute("INSERT OR IGNORE INTO user_task_code_attempt(task_id) SELECT id FROM user_tasks WHERE id=? AND run_id=? AND bot_id=? AND status='pending' AND EXISTS(SELECT 1 FROM runs WHERE id=? AND status='awaiting_user')",params![task.id,task.run_id,task.bot_id,task.run_id])? == 1, "Code entry was already attempted or this step expired. Check the computer instead of submitting again.");
        Ok(())
    }
    // Caller holds the screen lease after closing interactive control sessions.
    pub fn ready_user_task(&self, task: &UserTask, slot: i64, outcome: &str) -> Result<()> {
        ensure!(
            matches!(outcome, "done" | "skipped"),
            "Unknown subtask outcome"
        );
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        ensure!(
            tx.query_row(
                "SELECT slot FROM screens WHERE bot_id=?",
                [&task.bot_id],
                |r| r.get::<_, i64>(0)
            )? == slot,
            "Subtask belongs to another screen"
        );
        ensure!(tx.execute("UPDATE user_tasks SET status='ready',outcome=?1 WHERE id=?2 AND run_id=?3 AND bot_id=?4 AND status='pending' AND EXISTS(SELECT 1 FROM runs WHERE id=?3 AND status='awaiting_user')",params![outcome,task.id,task.run_id,task.bot_id])? == 1, "This subtask is no longer waiting");
        tx.execute("UPDATE screens SET takeover=0 WHERE slot=?", [slot])?;
        tx.commit()?;
        Ok(())
    }
    // Only the original scheduler may resume, after it has reacquired its screen lease.
    pub fn resume_user_task(&self, task: &UserTask, slot: i64) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        ensure!(
            tx.query_row(
                "SELECT takeover FROM screens WHERE slot=? AND bot_id=?",
                params![slot, task.bot_id],
                |r| r.get::<_, bool>(0)
            )? == false,
            "User still controls this screen"
        );
        ensure!(
            tx.execute(
                "UPDATE runs SET status='running' WHERE id=? AND bot_id=? AND status='awaiting_user'",
                params![task.run_id, task.bot_id]
            )? == 1,
            "This run is no longer waiting"
        );
        ensure!(
            tx.execute(
                "UPDATE user_tasks SET status='resumed' WHERE id=? AND run_id=? AND bot_id=? AND status='ready'",
                params![task.id, task.run_id, task.bot_id]
            )? == 1,
            "This subtask is no longer ready"
        );
        tx.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,'user_action_done',?,?)",
            params![
                task.run_id,
                serde_json::to_string(&json!({"id":task.id,"outcome":task.outcome}))?,
                db::now()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}
pub async fn request(app: &App, run: &Run, title: &str, instructions: &str) -> Result<Value> {
    request_auth(app, run, title, instructions, None).await
}
pub fn validate_authentication(auth: &Value) -> Result<()> {
    ensure!(auth.is_object(), "Authentication details must be an object");
    ensure!(auth.as_object().unwrap().keys().all(|k| ["service", "method", "destination", "submission", "code_length"].contains(&k.as_str())), "Unsupported authentication detail");
    let service = auth["service"].as_str().unwrap_or("");
    ensure!(!service.trim().is_empty() && service.len() <= 120 && !service.chars().any(char::is_control), "Name the sign-in service");
    ensure!(matches!(auth["method"].as_str(), Some("sms" | "email" | "authenticator" | "push" | "security_key" | "signin")), "Unsupported verification method");
    if let Some(length) = auth.get("code_length") {
        ensure!(length.as_u64().is_some_and(|n| (4..=16).contains(&n)), "Code length must be an integer from 4 to 16");
    }
    if let Some(submission) = auth.get("submission") {
        ensure!(matches!(submission.as_str(), Some("enter" | "automatic")), "Unsupported code submission method");
    }
    if let Some(destination) = auth.get("destination") {
        let value = destination.as_str().unwrap_or("");
        ensure!(value.len() <= 160 && !value.chars().any(char::is_control), "Use a short masked destination");
    }
    Ok(())
}
pub async fn request_auth(app: &App, run: &Run, title: &str, instructions: &str, authentication: Option<Value>) -> Result<Value> {
    let task = app.db.request_user_task_with_auth(run, title, instructions, authentication)?;
    loop {
        ensure!(!app.db.cancelled(&run.id), "Run cancelled");
        let current = app.db.user_task(&task.id)?;
        match current.status.as_str() {
            "resumed" => {
                return Ok(
                    json!({"text":if current.outcome == "done" { "The user completed the requested step (through the verification card or Done with subtask) and returned control. Continue this same task. Take a fresh screenshot and verify the state before acting; the user's confirmation alone does not prove authentication succeeded." } else { "The user skipped this subtask and returned control. Do not assume it succeeded. Explain the limitation and continue only work that does not depend on it." }}),
                );
            }
            "pending" | "ready" => {}
            _ => anyhow::bail!("This subtask expired. Do not continue it automatically."),
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime,
        tests::{app, bot},
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[test]
    fn authentication_metadata_persists_without_accepting_secrets() {
        let app = app(); let bot = bot(&app.db, "codex");
        app.db.queue(&bot.id, "Sign in", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let auth = json!({"service":"Example","method":"sms","destination":"SMS sent to ***-***-9090"});
        let task = app.db.request_user_task_with_auth(&run, "Verify sign-in", "Use the code card", Some(auth.clone())).unwrap();
        assert_eq!(app.db.user_task(&task.id).unwrap().authentication, Some(auth.clone()));
        assert_eq!(app.db.user_tasks().unwrap()[0].authentication, Some(auth));
        assert!(app.db.reserve_code_entry(&task).is_ok());
        assert!(app.db.reserve_code_entry(&task).is_err());
        assert!(validate_authentication(&json!({"service":"Example","method":"sms","code":"123456"})).is_err());
        assert!(validate_authentication(&json!({"service":"Example","method":"password"})).is_err());
        assert!(validate_authentication(&json!({"service":"","method":"push"})).is_err());
        for length in [json!(3),json!(17),json!(6.5),json!("6")] {
            assert!(validate_authentication(&json!({"service":"Example","method":"sms","code_length":length})).is_err());
        }
        for length in [6,8] { assert!(validate_authentication(&json!({"service":"Example","method":"sms","code_length":length})).is_ok()); }
    }
    #[tokio::test]
    async fn code_endpoint_rejects_wrong_task_invalid_code_and_expired_step_without_recording_input() {
        use std::future::IntoFuture;
        let app = app(); let bot = bot(&app.db, "codex");
        app.db.queue(&bot.id, "Verify", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let task = app.db.request_user_task_with_auth(&run, "Verify", "Use code", Some(json!({"service":"Example","method":"authenticator"}))).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/user-tasks/{}/code",listener.local_addr().unwrap(),task.id);
        let server = tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        for body in [json!({"bot_id":"wrong","run_id":run.id,"code":"A1B2C3"}),json!({"bot_id":bot.id,"run_id":run.id,"code":"bad code"})] {
            let response = client.post(&url).bearer_auth(&app.token).json(&body).send().await.unwrap();
            assert!(!response.status().is_success());
        }
        app.db.cancel(&run.id).unwrap();
        let response = client.post(&url).bearer_auth(&app.token).json(&json!({"bot_id":bot.id,"run_id":run.id,"code":"A1B2C3"})).send().await.unwrap();
        assert!(!response.status().is_success());
        assert!(!serde_json::to_string(&app.db.events(&run.id).unwrap()).unwrap().contains("A1B2C3"));
        server.abort();
    }
    async fn waiting(app: &App, run: &str) -> UserTask {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(task) = app.db.pending_user_task(run).unwrap() {
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    #[test]
    fn handoffs_require_current_images_and_post_resume_verification() {
        let app = app();
        let b = bot(&app.db, "codex");
        let id = app.db.queue(&b.id, "Visit test page", 0).unwrap();
        assert!(app.db.require_observed_handoff(&id).is_ok());
        app.db
            .event(
                &id,
                "tool_result",
                json!({"tool":"computer_click","failed":false}),
            )
            .unwrap();
        assert!(app.db.require_observed_handoff(&id).is_err());
        app.db
            .event(
                &id,
                "tool_result",
                json!({"tool":"computer_screenshot","has_image":false}),
            )
            .unwrap();
        assert!(app.db.require_observed_handoff(&id).is_err());
        app.db
            .event(
                &id,
                "tool_result",
                json!({"tool":"computer_screenshot","has_image":true,"failed":true}),
            )
            .unwrap();
        assert!(app.db.require_observed_handoff(&id).is_err());
        app.db
            .event(
                &id,
                "tool_result",
                json!({"tool":"computer_screenshot","has_image":true,"failed":false}),
            )
            .unwrap();
        assert!(app.db.require_observed_handoff(&id).is_ok());
        app.db
            .event(&id, "user_action_done", json!({"outcome":"done"}))
            .unwrap();
        assert!(app.db.handoff_needs_observation(&id).unwrap());
        let other = app.db.queue(&b.id, "Other task", 0).unwrap();
        app.db
            .event(
                &other,
                "tool_result",
                json!({"tool":"computer_screenshot","has_image":true}),
            )
            .unwrap();
        assert!(app.db.handoff_needs_observation(&id).unwrap());
        app.db
            .event(
                &id,
                "tool_result",
                json!({"tool":"computer_screenshot","has_image":true}),
            )
            .unwrap();
        assert!(!app.db.handoff_needs_observation(&id).unwrap());
        app.db
            .event(&id, "user_action_done", json!({"outcome":"skipped"}))
            .unwrap();
        assert!(app.db.handoff_needs_observation(&id).unwrap());
        app.db
            .event(
                &id,
                "tool_result",
                json!({"tool":"computer_screenshot","has_image":true}),
            )
            .unwrap();
        assert!(!app.db.handoff_needs_observation(&id).unwrap());
        app.db
            .event(&id, "tool_result", json!({"tool":"computer_key"}))
            .unwrap();
        assert!(app.db.require_observed_handoff(&id).is_err());
    }
    #[tokio::test]
    async fn human_takeover_pauses_deadline_and_resumes_the_same_call_once() {
        let mut app = app();
        Arc::get_mut(&mut app).unwrap().config.run_timeout_seconds = 1;
        let bot = bot(&app.db, "openrouter");
        let id = app
            .db
            .queue(&bot.id, "Continue after I sign in", 0)
            .unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        assert_eq!(run.id, id);
        let slot = app.db.screen(&bot.id).unwrap();
        let mut lease = None;
        drop(crate::desktop_sessions::enter(&app, &run, "computer_screenshot").await.unwrap());
        let continued = Arc::new(AtomicUsize::new(0));
        let owner = app.clone();
        let r = run.clone();
        let b = bot.clone();
        let count = continued.clone();
        let driver = tokio::spawn(async move {
            let work = async {
                let result = runtime::call_tool(&owner,&b,&r,"request_user_action",json!({"title":"Sign in","instructions":"Sign in in the open browser, then press Done with subtask."})).await?;
                assert!(
                    result["text"]
                        .as_str()
                        .unwrap()
                        .contains("fresh screenshot")
                );
                assert!(
                    owner.screen_lock(slot).try_lock_owned().is_ok(),
                    "Resuming non-desktop work must not reserve the screen"
                );
                assert!(!owner.db.screen_takeover(slot)?);
                count.fetch_add(1, Ordering::SeqCst);
                Ok("Continued the original task".into())
            };
            let (result, abrupt) = runtime::drive_run(&owner, &r, slot, &mut lease, work)
                .await
                .unwrap();
            assert!(!abrupt);
            assert!(result.is_ok());
            owner
                .db
                .finish(&r.id, "completed", &result.unwrap(), "")
                .unwrap();
        });
        let task = waiting(&app, &run.id).await;
        tokio::time::sleep(Duration::from_millis(160)).await;
        assert!(app.screen_lock(slot).try_lock_owned().is_ok());
        let next = app
            .db
            .queue(&bot.id, "Any progress on the original task?", 0)
            .unwrap();
        app.db.request_steering(&next, &run.id).unwrap();
        assert!(app.db.claim_bot(&bot.id).unwrap().is_none());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        let url = format!("{base}/api/user-tasks/{}/complete", task.id);
        let body = json!({"bot_id":bot.id,"run_id":run.id,"outcome":"done"});
        assert_eq!(
            client
                .get(format!("{base}/api/user-tasks"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client.post(&url).json(&body).send().await.unwrap().status(),
            401
        );
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&app.token)
                .json(&json!({"bot_id":"another","run_id":run.id}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(
            client
                .post(format!("{base}/api/takeover"))
                .bearer_auth(&app.token)
                .json(&json!({"bot_id":bot.id,"enabled":true}))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert!(app.db.screen_takeover(slot).unwrap());
        tokio::time::sleep(Duration::from_millis(1150)).await;
        assert!(
            !driver.is_finished(),
            "human time was charged to bot timeout"
        );
        assert_eq!(app.db.run(&run.id).unwrap().status, "awaiting_user");
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&app.token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&app.token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        tokio::time::timeout(Duration::from_secs(2), driver)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(continued.load(Ordering::SeqCst), 1);
        assert_eq!(app.db.user_task(&task.id).unwrap().status, "resumed");
        assert_eq!(app.db.run(&run.id).unwrap().status, "completed");
        assert_eq!(app.db.run(&next).unwrap().status, "steered");
        assert!(app.db.claim_bot(&bot.id).unwrap().is_none());
        assert_eq!(
            app.db
                .events(&run.id)
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "user_followups")
                .count(),
            1
        );
        // A stale Done click cannot release later manual control.
        app.db.screen_set_takeover(slot, true).unwrap();
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&app.token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert!(app.db.screen_takeover(slot).unwrap());
        assert_eq!(continued.load(Ordering::SeqCst), 1);
        server.abort();
    }
    #[tokio::test]
    async fn cancelled_human_subtask_cannot_resume_or_steal_manual_control() {
        let app = app();
        let bot = bot(&app.db, "codex");
        app.db.queue(&bot.id, "Wait for me", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let slot = app.db.screen(&bot.id).unwrap();
        let mut lease = Some(app.screen_lock(slot).lock_owned().await);
        let owner = app.clone();
        let r = run.clone();
        let driver = tokio::spawn(async move {
            let work = async {
                request(&owner, &r, "Sign in", "Use the browser").await?;
                panic!("cancelled task resumed")
            };
            let (result, abrupt) = runtime::drive_run(&owner, &r, slot, &mut lease, work)
                .await
                .unwrap();
            assert!(result.is_err());
            assert!(!abrupt);
            owner.db.finish(&r.id, "cancelled", "", "Stopped").unwrap();
        });
        let task = waiting(&app, &run.id).await;
        tokio::time::sleep(Duration::from_millis(160)).await;
        app.db.screen_set_takeover(slot, true).unwrap();
        app.db.cancel(&run.id).unwrap();
        tokio::time::timeout(Duration::from_secs(2), driver)
            .await
            .unwrap()
            .unwrap();
        assert!(app.db.screen_takeover(slot).unwrap());
        assert_eq!(app.db.user_task(&task.id).unwrap().status, "expired");
        assert!(app.db.ready_user_task(&task, slot, "done").is_err());
        assert!(app.db.resume_user_task(&task, slot).is_err());
        assert!(app.db.screen_takeover(slot).unwrap());
    }
    #[test]
    fn waiting_tasks_remain_visible_after_history_limits() {
        let app = app();
        let b = bot(&app.db, "codex");
        app.db.queue(&b.id, "Human step", 0).unwrap();
        let run = app.db.claim_bot(&b.id).unwrap().unwrap();
        let task = app
            .db
            .request_user_task(&run, "Sign in", "Use the browser")
            .unwrap();
        let other = bot(&app.db, "codex");
        for _ in 0..505 {
            let id = app.db.queue(&other.id, "Later work", 0).unwrap();
            app.db
                .0
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO user_tasks VALUES(?,?,?,?,?,'resumed','done',?)",
                    params![db::id(), id, other.id, "Old", "Complete", db::now() + 1],
                )
                .unwrap();
            app.db.finish(&id, "completed", "Done", "").unwrap();
        }
        assert_eq!(app.db.runs(None).unwrap().len(), 101);
        assert!(
            app.db
                .runs(None)
                .unwrap()
                .iter()
                .any(|r| r.id == run.id && r.status == "awaiting_user")
        );
        assert_eq!(app.db.user_tasks().unwrap()[0].id, task.id);
    }
    #[test]
    fn resume_cannot_cross_bot_or_run_boundary() {
        let app = app();
        let first = bot(&app.db, "codex");
        let second = bot(&app.db, "openrouter");
        app.db.queue(&first.id, "First", 0).unwrap();
        let first_run = app.db.claim_bot(&first.id).unwrap().unwrap();
        let first_task = app
            .db
            .request_user_task(&first_run, "Human step", "Use the browser")
            .unwrap();
        let first_slot = app.db.screen(&first.id).unwrap();
        app.db.screen_set_takeover(first_slot, true).unwrap();
        app.db
            .ready_user_task(&first_task, first_slot, "done")
            .unwrap();

        app.db.queue(&second.id, "Second", 0).unwrap();
        let second_run = app.db.claim_bot(&second.id).unwrap().unwrap();
        let mut forged = first_task.clone();
        forged.run_id = second_run.id.clone();
        forged.bot_id = second.id.clone();
        assert!(
            app.db
                .resume_user_task(&forged, app.db.screen(&second.id).unwrap())
                .is_err()
        );
        assert_eq!(app.db.run(&first_run.id).unwrap().status, "awaiting_user");
        assert_eq!(app.db.run(&second_run.id).unwrap().status, "running");
        assert_eq!(app.db.user_task(&first_task.id).unwrap().status, "ready");
    }
    #[tokio::test]
    async fn post_handoff_blocks_computer_and_guest_tools_before_vm_access() {
        let app = app();
        let mut bot = bot(&app.db, "codex");
        bot.auto_approve = true;
        bot.approval_mode = "auto".into();
        app.db.save_bot_preferences(&bot, true).unwrap();
        app.db
            .queue(&bot.id, "Continue after human sign-in", 0)
            .unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        // A resumed handoff is represented by its durable completion event.
        // No screenshot has been recorded for this run yet.
        app.db
            .event(&run.id, "user_action_done", json!({"outcome":"done"}))
            .unwrap();
        assert!(app.db.handoff_needs_observation(&run.id).unwrap());
        for (name, args) in [
            ("computer_open_url", json!({"url":"https://example.test"})),
            ("computer_click", json!({"x":1,"y":1})),
            ("computer_type", json!({"text":"fixture"})),
            ("computer_key", json!({"key":"ENTER"})),
            ("computer_scroll", json!({"x":1,"y":1,"delta_y":1})),
            ("guest_exec", json!({"command":"true"})),
            ("local_exec", json!({"command":"true"})),
        ] {
            let result = runtime::call_tool(&app, &bot, &run, name, args)
                .await
                .unwrap();
            assert_eq!(result["failed"], true, "{name} crossed the handoff gate");
        }
        assert!(app.db.events(&run.id).unwrap().iter().all(|event| {
            !(event["kind"] == "tool_started"
                && [
                    "computer_open_url",
                    "computer_click",
                    "computer_type",
                    "computer_key",
                    "computer_scroll",
                    "guest_exec",
                    "local_exec",
                ]
                .contains(&event["body"]["tool"].as_str().unwrap_or("")))
        }));
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
    fn restart_expires_human_subtask_without_replaying_it() {
        let path = std::env::temp_dir().join(format!("kindred-human-{}.db", db::id()));
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = bot(&db, "codex");
        db.queue(&bot.id, "Sign in", 0).unwrap();
        let run = db.claim_bot(&bot.id).unwrap().unwrap();
        let slot = db.screen(&bot.id).unwrap();
        let task = db
            .request_user_task(&run, "Sign in", "Use the browser")
            .unwrap();
        assert!(db.request_user_task(&run, "Duplicate", "No").is_err());
        db.screen_set_takeover(slot, true).unwrap();
        drop(db);
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(db.user_task(&task.id).unwrap().status, "expired");
        assert_eq!(db.run(&run.id).unwrap().status, "interrupted");
        assert!(db.screen_takeover(slot).unwrap());
        assert!(db.pending_user_task(&run.id).unwrap().is_none());
        drop(db);
        std::fs::remove_file(&path).unwrap();
        let _ = std::fs::remove_file(format!("{}.lock", path.display()));
    }
}
