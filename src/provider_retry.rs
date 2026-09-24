//! Bounded retries stay in the same task, model and approval context.
use crate::{db::Run, runtime::App};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{future::Future, time::Duration};

pub const EXHAUSTED: &str = "Connection issue - 5 retries failed.";
const DELAYS: [u64; 5] = [5, 10, 20, 40, 60];

fn transient(error: &anyhow::Error) -> bool {
    error.chain().any(|e| {
        let text = e.to_string();
        text == "The subscription provider could not complete this task. Check its official CLI connection and quota; no alternate provider was used."
            || text == "Subscription provider timed out"
            || text == "Codex request timed out"
            || text == "Kindred could not keep an SSH connection to the bot computer. Check that the computer is running and its SSH configuration is valid, then retry sign-in."
            || text == "Codex stopped responding on the bot computer. Try Check connection or sign in again; if it repeats, ask the server administrator to check the guest's Codex installation."
            || text == "Codex turn ended with status failed"
            || text.starts_with("Codex reported an error:")
            || text == "Pi harness disconnected. Check the pinned Node runtime and harness installation; no alternate provider was used."
            || text.starts_with("OpenRouter connection failed")
            || text.starts_with("The custom provider connection failed")
            || text.starts_with("OpenRouter returned an incomplete or failed model response")
            || text.starts_with("The custom provider returned an incomplete or failed model response")
            || ["OpenRouter returned HTTP ", "The custom provider returned HTTP "].iter().any(|prefix| text.strip_prefix(prefix).is_some_and(|rest| rest.starts_with("429.") || rest.starts_with('5')))
            || e.downcast_ref::<reqwest::Error>().is_some_and(|e| e.is_timeout() || e.is_connect() || e.status().is_some_and(|s|s.as_u16()==429||s.is_server_error()))
            || e.downcast_ref::<std::io::Error>().is_some_and(|e| matches!(e.kind(),std::io::ErrorKind::TimedOut|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::ConnectionAborted|std::io::ErrorKind::BrokenPipe|std::io::ErrorKind::UnexpectedEof))
    })
}

fn can_resume(app: &App, run: &Run) -> Result<bool> {
    // A disconnected provider is not evidence that an external write failed.
    // Do not replay a turn while a tool lacks a durable execution receipt.
    let c = app.db.0.lock().unwrap();
    let uncertain: bool=c.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='tool_result' AND (json_extract(body,'$.timed_out')=1 OR json_extract(body,'$.stopped')=1))",[&run.id],|r|r.get(0))?;
    if uncertain {
        return Ok(false);
    }
    Ok(!c.query_row("SELECT EXISTS(SELECT 1 FROM events requested WHERE requested.run_id=? AND requested.kind='tool_requested' AND NOT EXISTS(SELECT 1 FROM events result WHERE result.run_id=requested.run_id AND result.kind='tool_result' AND result.seq>requested.seq AND json_extract(result.body,'$.call_id')=json_extract(requested.body,'$.call_id'))) OR EXISTS(SELECT 1 FROM connector_artifacts WHERE run_id=? AND status IN ('preparing','pending','approved','ready','executing','interrupted','failed'))",[&run.id,&run.id],|r|r.get::<_,bool>(0))?)
}

pub fn check_tool_budget(app: &App, run: &Run) -> Result<()> {
    if app.config.max_steps == 0 { return Ok(()); }
    let count: usize = app.db.0.lock().unwrap().query_row(
        "SELECT count(*) FROM events WHERE run_id=? AND kind='tool_requested'",
        [&run.id],
        |r| r.get(0),
    )?;
    ensure!(
        count < app.config.max_steps,
        "This task reached its action limit. Completed actions are preserved in Activity."
    );
    Ok(())
}

pub async fn run<F, Fut>(app: &App, run: &Run, attempt: F) -> Result<String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<String>>,
{
    retry(app, run, attempt, &DELAYS.map(Duration::from_secs)).await
}

async fn retry<F, Fut>(
    app: &App,
    run: &Run,
    mut attempt: F,
    delays: &[Duration; 5],
) -> Result<String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<String>>,
{
    for number in 0..=5 {
        ensure!(
            !app.account_disabled() && !app.db.cancelled(&run.id),
            "Run cancelled"
        );
        if number > 0 {
            app.db.event(&run.id,"provider_retry",json!({"attempt":number,"limit":5,"phase":"retrying","delay_seconds":delays[number-1].as_secs()}))?;
            tokio::time::sleep(delays[number - 1]).await;
            ensure!(
                !app.account_disabled() && !app.db.cancelled(&run.id),
                "Run cancelled"
            );
        }
        match attempt().await {
            Ok(output) => {
                if number > 0 {
                    app.db.event(
                        &run.id,
                        "provider_retry",
                        json!({"attempt":number,"limit":5,"phase":"recovered"}),
                    )?;
                }
                return Ok(output);
            }
            Err(error) => {
                if !transient(&error) || !can_resume(app, run)? || app.db.turn_deferred(&run.id)? {
                    return Err(error);
                }
                app.db.event(
                    &run.id,
                    "provider_attempt_failed",
                    json!({"attempt":number,"error":error.to_string()}),
                )?;
                if number == 5 {
                    app.db.event(
                        &run.id,
                        "provider_retry",
                        json!({"attempt":5,"limit":5,"phase":"exhausted"}),
                    )?;
                    bail!(EXHAUSTED);
                }
            }
        }
    }
    unreachable!()
}

pub fn progress(app: &App, run: &Run) -> Result<Option<Value>> {
    if run.status != "running" {
        return Ok(None);
    }
    let c = app.db.0.lock().unwrap();
    // Once a retry produces real work, return to the ordinary work indicator.
    let newer_work: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND (kind IN ('tool_requested','tool_result') OR (kind='assistant' AND COALESCE(json_extract(body,'$.status_notice'),'')!='provider_error')) AND seq>COALESCE((SELECT max(seq) FROM events WHERE run_id=? AND kind='provider_retry'),0))",[&run.id,&run.id],|r|r.get(0))?;
    if newer_work {
        return Ok(None);
    }
    let value = crate::task_recovery::metadata(&c, &run.id, "provider_retry")?;
    Ok(value.filter(|v| v["phase"] == "retrying"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests;
    use std::{cell::Cell, future::ready};
    fn blip() -> anyhow::Error {
        anyhow::anyhow!("Subscription provider timed out")
    }
    #[tokio::test]
    async fn retries_recover_in_same_run_and_exhaust_exactly_five() {
        for failures in [1, 4, 6] {
            let app = tests::app();
            let bot = tests::bot(&app.db, "claude-code");
            let id = app.db.queue(&bot.id, "Keep my work", 0).unwrap();
            let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
            let calls = Cell::new(0);
            let result = retry(
                &app,
                &run,
                || {
                    calls.set(calls.get() + 1);
                    ready(if calls.get() <= failures {
                        Err(blip())
                    } else {
                        Ok("Recovered".into())
                    })
                },
                &[Duration::ZERO; 5],
            )
            .await;
            assert_eq!(calls.get(), (failures + 1).min(6));
            assert_eq!(app.db.runs(None).unwrap().len(), 1);
            assert_eq!(app.db.run(&id).unwrap().status, "running");
            if failures == 6 {
                assert_eq!(result.unwrap_err().to_string(), EXHAUSTED);
            } else {
                assert_eq!(result.unwrap(), "Recovered");
            }
            let progress =
                crate::task_recovery::metadata(&app.db.0.lock().unwrap(), &id, "provider_retry")
                    .unwrap()
                    .unwrap();
            assert_eq!(
                progress["phase"],
                if failures == 6 {
                    "exhausted"
                } else {
                    "recovered"
                }
            );
        }
    }
    #[tokio::test]
    async fn no_replay_of_unknown_tool_outcomes_or_permanent_errors() {
        for incomplete in [false, true] {
            let app = tests::app();
            let bot = tests::bot(&app.db, "claude-code");
            let id = app.db.queue(&bot.id, "Send once", 0).unwrap();
            let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
            if incomplete {
                app.db
                    .event(
                        &id,
                        "tool_requested",
                        json!({"tool":"connector_execute","call_id":"send"}),
                    )
                    .unwrap();
            }
            let calls = Cell::new(0);
            assert!(
                retry(
                    &app,
                    &run,
                    || {
                        calls.set(calls.get() + 1);
                        ready(Err(if incomplete {
                            blip()
                        } else {
                            anyhow::anyhow!("Invalid provider tool request")
                        }))
                    },
                    &[Duration::ZERO; 5]
                )
                .await
                .is_err()
            );
            assert_eq!(calls.get(), 1);
        }
    }
    #[tokio::test]
    async fn cancellation_during_backoff_never_starts_another_attempt() {
        let app = tests::app();
        let bot = tests::bot(&app.db, "claude-code");
        let id = app.db.queue(&bot.id, "Try", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let calls = Cell::new(0);
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            app.db.cancel(&id).unwrap();
        };
        let delays = [Duration::from_millis(20); 5];
        let work = retry(
            &app,
            &run,
            || {
                calls.set(calls.get() + 1);
                ready(Err(blip()))
            },
            &delays,
        );
        let (result, ()) = tokio::join!(work, cancel);
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }
    #[tokio::test]
    async fn retry_preserves_context_usage_and_action_budget() {
        let mut app = tests::app();
        std::sync::Arc::get_mut(&mut app).unwrap().config.max_steps = 24;
        let bot = tests::bot(&app.db, "claude-code");
        let id = app.db.queue(&bot.id, "Send the report once", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        app.db.event(&id,"tool_requested",json!({"tool":"connector_execute","call_id":"sent","args":{"tool_slug":"send_email"}})).unwrap();
        app.db.event(&id,"tool_result",json!({"tool":"connector_execute","call_id":"sent","text":"Sent receipt message-123","failed":false})).unwrap();
        let usage = json!({"request_id":1,"input_tokens":100,"output_tokens":20,"cached_tokens":0,"cost":null,"cost_source":"subscription"});
        crate::provider_accounts::record(&app, &bot, &run, &usage).unwrap();
        let calls = Cell::new(0);
        retry(&app,&run,||{
            calls.set(calls.get()+1);
            if calls.get()==1 {return ready(Err(blip()));}
            let context=crate::runtime::instructions_for(&app,&bot,&run,&crate::runtime::tool_specs_for(&app,&bot),None).unwrap();
            assert!(context.contains("Sent receipt message-123"));assert!(context.contains("original request does not authorize repeating completed actions"));
            assert!(progress(&app,&app.db.run(&id).unwrap()).unwrap().is_some());
            crate::provider_accounts::record(&app,&bot,&run,&usage).unwrap();
            app.db.event(&id,"assistant",json!({"text":"The report was already sent. I will summarize the delivery."})).unwrap();
            assert!(progress(&app,&app.db.run(&id).unwrap()).unwrap().is_none());
            ready(Ok("Done".into()))
        },&[Duration::ZERO;5]).await.unwrap();
        let counts: (i64, i64) = app
            .db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*),sum(input_tokens) FROM provider_usage WHERE run_id=?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (2, 200));
        assert_eq!(
            app.db
                .events(&id)
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "tool_requested")
                .count(),
            1
        );
        for i in 1..app.config.max_steps {
            app.db
                .event(&id, "tool_requested", json!({"call_id":i}))
                .unwrap();
        }
        assert!(check_tool_budget(&app, &run).is_err());
    }

    #[test]
    fn classifier_excludes_configuration_and_budget_failures() {
        for message in [
            "Tool-step budget exhausted",
            "Choose the provider default thinking level",
            "OpenRouter returned HTTP 401. Check credentials",
            "Invalid provider tool request",
            "Run cancelled",
        ] {
            assert!(!transient(&anyhow::anyhow!(message)));
        }
        for message in [
            "OpenRouter returned HTTP 503. Unavailable",
            "Codex request timed out",
            "Codex turn ended with status failed",
        ] {
            assert!(transient(&anyhow::anyhow!(message)));
        }
    }
}
