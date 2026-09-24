//! Pi owns the non-Codex agent loop; Kindred owns tools, approvals and VM leases.
//! A bounded stdin/stdout bridge keeps provider credentials out of command lines.
use crate::{
    db::{Bot, Run},
    runtime::{self, App},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

const MAX_FRAME: usize = 8 * 1024 * 1024;
fn valid_sdk_version(value: &Value) -> bool {
    value.as_str().is_some_and(|version| {
        let parts: Vec<_> = version.split('.').collect();
        parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.len() <= 8 && part.bytes().all(|b| b.is_ascii_digit()))
    })
}
const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

pub async fn openrouter(
    app: &App,
    bot: &Bot,
    run: &Run,
    key: &str,
    model_info: Value,
) -> Result<String> {
    run_worker(
        app,
        bot,
        run,
        key,
        model_info,
        ENDPOINT,
        false,
        &app.config.pi,
    )
    .await
}

pub async fn custom(
    app: &App,
    bot: &Bot,
    run: &Run,
    key: &str,
    info: Value,
    endpoint: &str,
) -> Result<String> {
    run_worker(app, bot, run, key, info, endpoint, false, &app.config.pi).await
}

pub(crate) async fn send(input: &mut tokio::process::ChildStdin, frame: Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(&frame)?;
    ensure!(
        bytes.len() <= MAX_FRAME,
        "Pi bridge input exceeds the frame limit"
    );
    bytes.push(b'\n');
    input.write_all(&bytes).await?;
    input.flush().await?;
    Ok(())
}

pub(crate) async fn read(output: &mut BufReader<tokio::process::ChildStdout>) -> Result<Value> {
    let mut line = Vec::new();
    loop {
        let available = output.fill_buf().await?;
        ensure!(
            !available.is_empty(),
            "Pi harness disconnected. Check the pinned Node runtime and harness installation; no alternate provider was used."
        );
        let count = available
            .iter()
            .position(|&byte| byte == b'\n')
            .map(|n| n + 1)
            .unwrap_or(available.len());
        ensure!(
            line.len() + count <= MAX_FRAME,
            "Pi bridge output exceeds the frame limit"
        );
        let done = available[count - 1] == b'\n';
        line.extend_from_slice(&available[..count]);
        output.consume(count);
        if done {
            return serde_json::from_slice(&line).context("Invalid Pi bridge frame");
        }
    }
}

fn provider_error(frame: &Value) -> String {
    match frame["code"].as_str().unwrap_or("") {
        "provider_http" => {
            let status = frame["status"].as_u64().filter(|s| (100..=599).contains(s)).unwrap_or(0);
            format!("OpenRouter returned HTTP {status}. Verify model availability, balance and ZDR eligibility; no alternate provider was used.")
        }
        "budget" => "Tool-step budget exhausted. Review activity and continue with a narrower task.".into(),
        "truncated" => "Model output was truncated at its token limit.".into(),
        "aborted" => "Pi task was cancelled.".into(),
        "request_limit" | "response_limit" | "output_limit" | "frame_limit" => "Pi task reached a request, response or output size limit. Continue with a narrower task.".into(),
        "reasoning" => "The selected model does not support this thinking level. Update the bot's settings.".into(),
        "transport" => "OpenRouter connection failed or redirected. No alternate provider was used.".into(),
        "provider_response" => "OpenRouter returned an incomplete or failed model response. No alternate provider was used.".into(),
        _ => "Pi could not complete this task. Check the selected model and harness installation; no alternate provider was used.".into(),
    }
}

async fn run_worker(
    app: &App,
    bot: &Bot,
    run: &Run,
    key: &str,
    model_info: Value,
    endpoint: &str,
    fixture: bool,
    config: &crate::config::Pi,
) -> Result<String> {
    ensure!(
        matches!(
            bot.reasoning_effort.as_str(),
            "" | "low" | "medium" | "high"
        ),
        "Choose Low, Medium, High or the provider default thinking level"
    );
    let mut command = Command::new(&config.node_binary);
    command
        .arg(&config.worker_script)
        .env_clear()
        .env("PI_OFFLINE", "1")
        .env("NO_COLOR", "1");
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true)
        .spawn().context("Start the Pi harness. Install the pinned Node runtime and harness dependencies; no alternate provider was used")?;
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let tools = runtime::tool_specs_for(app, bot);
    let allowed: HashSet<String> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    send(&mut input, json!({
        "type":"start", "protocol":1, "provider":bot.provider, "model":bot.model,
        "session_id":format!("{}:{}",bot.id,run.chat_id),
        "model_info":model_info, "api_key":key, "endpoint":endpoint, "fixture":fixture,
        "request_timeout_ms":app.config.run_timeout_seconds * 1000,
        "instructions":runtime::instructions_for(app, bot, run, &tools, model_info["context_window"].as_u64())?, "prompt":run.prompt, "tools":tools,
        "reasoning_effort":bot.reasoning_effort, "require_zdr":app.config.openrouter.require_zdr,
        "max_steps":app.config.max_steps
    })).await?;
    let mut ready = false;
    let mut result = String::new();
    let mut call_ids = HashSet::new();
    loop {
        let frame = if ready {
            read(&mut output).await?
        } else {
            tokio::time::timeout(Duration::from_secs(45), read(&mut output))
                .await
                .context("Pi harness startup timed out")??
        };
        match frame["type"].as_str().unwrap_or("") {
            "error" => bail!(
                "{}",
                if bot.provider.starts_with("custom-") {
                    provider_error(&frame)
                        .replace("OpenRouter", "The custom provider")
                        .replace("balance and ZDR eligibility", "credentials and quota")
                } else if matches!(bot.provider.as_str(), "opencode" | "opencode-go") {
                    provider_error(&frame).replace("OpenRouter", if bot.provider == "opencode-go" {"OpenCode Go"} else {"OpenCode Zen"}).replace("balance and ZDR eligibility", "API key, plan and quota")
                } else {
                    provider_error(&frame)
                }
            ),
            "ready" => {
                ensure!(
                    !ready
                        && frame["protocol"] == 1
                        && valid_sdk_version(&frame["sdk_version"])
                        && frame["provider"] == bot.provider
                        && frame["model"] == bot.model,
                    "Unexpected Pi harness version or model selection"
                );
                ready = true;
                app.db.event(&run.id, "model_selected", json!({"provider":bot.provider,"model":bot.model,"reasoning_effort":bot.reasoning_effort,"harness":"pi","harness_version":frame["sdk_version"]}))?;
            }
            "progress" if ready => {
                let state = frame["state"].as_str().unwrap_or("");
                ensure!(
                    matches!(state, "waiting" | "thinking" | "writing" | "preparing"),
                    "Invalid Pi progress state"
                );
                app.db
                    .event(&run.id, "model_progress", json!({"state":state}))?;
            }
            "assistant" if ready => {
                let text = runtime::string(&frame, "text")?;
                ensure!(
                    result.len() + text.len() <= 4 * 1024 * 1024,
                    "Pi assistant output limit reached"
                );
                result.push_str(text);
                result.push('\n');
                app.db.event(
                    &run.id,
                    "assistant",
                    json!({"text":text,"phase":frame["phase"]}),
                )?;
            }
            "tool_call" if ready => {
                ensure!(!app.db.cancelled(&run.id), "run cancelled");
                let id = runtime::string(&frame, "id")?;
                let name = runtime::string(&frame, "name")?;
                ensure!(
                    id.len() <= 512
                        && !id.chars().any(char::is_control)
                        && call_ids.insert(id.to_owned()),
                    "Pi repeated or returned an invalid tool call ID"
                );
                ensure!(
                    (app.config.max_steps == 0 || call_ids.len() <= app.config.max_steps),
                    "Tool-step budget exhausted"
                );
                ensure!(
                    allowed.contains(name),
                    "Pi requested a tool outside Kindred's tool set"
                );
                let tool_result = runtime::call_tool(app, bot, run, name, frame["args"].clone())
                    .await
                    .unwrap_or_else(|e| json!({"text":e.to_string(),"failed":true}));
                let end_turn = tool_result["deferred_question"] == true
                    || tool_result["finish_quietly"] == true
                    || tool_result["deferred_process"] == true;
                if end_turn {
                    // The question card or quiet-completion receipt is the outcome.
                    // Progress already lives in events; it is never a final reply.
                    return Ok(String::new());
                }
                send(
                    &mut input,
                    json!({"type":"tool_result","id":id,"result":tool_result}),
                )
                .await?;
            }
            "usage" if ready => {
                crate::provider_accounts::record(app, bot, run, &frame)?;
            }
            "context" if ready => {
                if matches!(
                    frame["state"].as_str(),
                    Some("compacting" | "compacted" | "failed")
                ) {
                    app.db
                        .event(&run.id, "context", json!({"state":frame["state"],"summary":frame["summary"].as_str().map(|s|crate::runtime::bounded(s,32000))}))?;
                }
            }
            "complete" if ready => {
                ensure!(
                    frame["output"].as_str().unwrap_or("").trim_end() == result.trim_end(),
                    "Pi completion did not match its emitted result"
                );
                ensure!(
                    !result.trim().is_empty()
                        || app
                            .db
                            .events(&run.id)?
                            .iter()
                            .any(|e| e["kind"] == "tool_result"
                                && e["body"]["tool"] == "react_to_message"
                                && e["body"]["failed"] != true),
                    "Model completed without a result"
                );
                let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
                    .await
                    .context("Pi harness did not exit after completion")??;
                ensure!(status.success(), "Pi harness exited unsuccessfully");
                // Earlier assistant messages describe tool work; don't concatenate
                // acknowledgements into the final conversational answer.
                let answer = app
                    .db
                    .events(&run.id)?
                    .into_iter()
                    .rev()
                    .find(|e| e["kind"] == "assistant")
                    .and_then(|e| e["body"]["text"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                return Ok(answer);
            }
            _ => bail!("Unexpected Pi bridge frame"),
        }
    }
}

#[cfg(test)]
pub async fn fixture(app: &App, bot: &Bot, run: &Run, key: &str, endpoint: &str) -> Result<String> {
    let config = crate::config::Pi {
        node_binary: std::env::var("KINDRED_PI_TEST_NODE").unwrap_or_else(|_| "node".into()),
        worker_script: std::env::var("KINDRED_PI_TEST_WORKER")
            .unwrap_or_else(|_| format!("{}/harness/pi/worker.mjs", env!("CARGO_MANIFEST_DIR"))),
    };
    let info = json!({"id":bot.model,"reasoning":true,"vision":true,"context_window":128000,"max_tokens":4096});
    run_worker(app, bot, run, key, info, endpoint, true, &config).await
}

#[cfg(test)]
pub fn sse_response(value: Value) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut message = value["choices"][0]["message"].clone();
    if let Some(calls) = message["tool_calls"].as_array_mut() {
        for (index, call) in calls.iter_mut().enumerate() {
            call["index"] = json!(index);
        }
    }
    let chunk = |delta: Value, finish: Value| json!({"id":"test-completion","object":"chat.completion.chunk","created":1,"model":"test/model","choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk(message, Value::Null),
        chunk(json!({}), value["choices"][0]["finish_reason"].clone())
    );
    ([("content-type", "text/event-stream")], body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    async fn tool_server(
        name: &str,
        args: Value,
    ) -> (
        String,
        tokio::task::JoinHandle<std::io::Result<()>>,
        Arc<AtomicUsize>,
    ) {
        use axum::{Json, Router, extract::State, routing::post};
        #[derive(Clone)]
        struct Mock {
            name: String,
            args: Value,
            calls: Arc<AtomicUsize>,
        }
        async fn respond(
            State(state): State<Mock>,
            Json(body): Json<Value>,
        ) -> axum::response::Response {
            if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                sse_response(
                    json!({"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"action_1","type":"function","function":{"name":state.name,"arguments":state.args.to_string()}}]},"finish_reason":"tool_calls"}]}),
                )
            } else {
                assert!(body["messages"].to_string().contains("declined"));
                sse_response(
                    json!({"choices":[{"message":{"role":"assistant","content":"The action was declined."},"finish_reason":"stop"}]}),
                )
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/chat/completions", post(respond))
                    .with_state(Mock {
                        name: name.into(),
                        args,
                        calls: calls.clone(),
                    }),
            )
            .into_future(),
        );
        (endpoint, server, calls)
    }

    #[tokio::test]
    async fn pi_choice_cards_end_the_check_and_receive_the_actual_answer_on_continuation() {
        use axum::{Json, Router, extract::State, routing::post};
        async fn respond(
            State(calls): State<Arc<AtomicUsize>>,
            Json(body): Json<Value>,
        ) -> axum::response::Response {
            let turn = calls.fetch_add(1, Ordering::SeqCst);
            if turn == 0 {
                assert!(body["messages"].to_string().contains("ask_question"));
                sse_response(
                    json!({"choices":[{"message":{"role":"assistant","content":"Choose what to do with the trial.\n\n","tool_calls":[{"id":"decision","type":"function","function":{"name":"ask_question","arguments":json!({"topic_key":"synthetic-trial","question":"Keep the trial?","context":"Synthetic trial ends Wednesday. https://example.com/billing","options":["Add a card","Let it lapse","I'll do it myself"]}).to_string()}}]},"finish_reason":"tool_calls"}]}),
                )
            } else {
                assert_eq!(turn, 1);
                assert!(
                    body["messages"]
                        .to_string()
                        .contains("USER'S RESPONSE: I'll do it myself")
                );
                sse_response(
                    json!({"choices":[{"message":{"role":"assistant","content":"Here is the billing link: https://example.com/billing. I will leave it to you."},"finish_reason":"stop"}]}),
                )
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/chat/completions", post(respond))
                    .with_state(calls.clone()),
            )
            .into_future(),
        );
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "openrouter");
        app.db
            .queue(&bot.id, "Review a synthetic trial email", 0)
            .unwrap();
        let run = app.db.claim().unwrap().unwrap();
        let output = fixture(&app, &bot, &run, "test-key", &endpoint)
            .await
            .unwrap();
        assert!(output.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        app.db.finish(&run.id, "completed", &output, "").unwrap();
        app.db.chat_complete(&run).unwrap();
        let messages = app.db.chat_messages(&run.chat_id).unwrap();
        assert_eq!(
            messages.iter().filter(|m| m["kind"] == "assistant").count(),
            1
        );
        assert_eq!(
            messages.iter().filter(|m| m["kind"] == "question").count(),
            1
        );
        assert!(!messages.iter().any(|m| m["kind"] == "result"));
        let id = app
            .db
            .decision_context(&bot.id, &run.chat_id, None)
            .unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let saved = app
            .db
            .answer_question(
                &id,
                crate::questions::Answer {
                    selected: Some(2),
                    custom: None,
                },
            )
            .unwrap();
        let follow = app.db.claim().unwrap().unwrap();
        assert_eq!(follow.id, saved.continuation_run_id);
        assert_eq!(
            fixture(&app, &bot, &follow, "test-key", &endpoint)
                .await
                .unwrap()
                .trim(),
            "Here is the billing link: https://example.com/billing. I will leave it to you."
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }
    #[tokio::test]
    async fn pi_approval_denial_returns_to_the_model_without_guest_execution() {
        let (endpoint, server, calls) = tool_server(
            "guest_exec",
            json!({"command":"echo never","action_scope":"external"}),
        )
        .await;
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "openrouter");
        app.db
            .queue(&bot.id, "An action needing approval", 0)
            .unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let owner = app.clone();
        let active = run.clone();
        let work =
            tokio::spawn(
                async move { fixture(&owner, &bot, &active, "test-key", &endpoint).await },
            );
        let approval = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(approval) = app.db.approvals().unwrap().first().cloned() {
                    break approval;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(approval["tool"], "guest_exec");
        app.db
            .decide(approval["id"].as_str().unwrap(), false)
            .unwrap();
        assert_eq!(
            work.await.unwrap().unwrap().trim(),
            "The action was declined."
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let events = app.db.events(&run.id).unwrap();
        assert!(!events.iter().any(|event| event["kind"] == "tool_started"));
        assert!(
            events
                .iter()
                .any(|event| event["kind"] == "tool_result" && event["body"]["failed"] == true)
        );
        server.abort();
    }

    #[tokio::test]
    async fn pi_cancellation_during_human_step_drops_the_worker_and_never_continues() {
        let (endpoint, server, calls) = tool_server(
            "request_user_action",
            json!({"title":"Sign in","instructions":"Use the browser"}),
        )
        .await;
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "openrouter");
        app.db.queue(&bot.id, "Wait for my sign-in", 0).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let slot = app.db.screen(&bot.id).unwrap();
        let mut lease = Some(app.screen_lock(slot).lock_owned().await);
        let owner = app.clone();
        let active = run.clone();
        let work = tokio::spawn(async move {
            let (result, abrupt) = runtime::drive_run(
                &owner,
                &active,
                slot,
                &mut lease,
                fixture(&owner, &bot, &active, "test-key", &endpoint),
            )
            .await
            .unwrap();
            assert!(result.is_err());
            assert!(!abrupt);
            owner
                .db
                .finish(&active.id, "cancelled", "", "Stopped")
                .unwrap();
        });
        let human = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(task) = app.db.pending_user_task(&run.id).unwrap() {
                    break task;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(160)).await;
        assert!(app.screen_lock(slot).try_lock().is_ok());
        app.db.screen_set_takeover(slot, true).unwrap();
        app.db.cancel(&run.id).unwrap();
        tokio::time::timeout(Duration::from_secs(3), work)
            .await
            .unwrap()
            .unwrap();
        assert!(app.db.ready_user_task(&human, slot, "done").is_err());
        assert!(app.db.screen_takeover(slot).unwrap());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(app.db.run(&run.id).unwrap().status, "cancelled");
        server.abort();
    }
}

#[cfg(test)]
mod workflow_delivery_tests {
    use super::*;
    use axum::{Json, Router, extract::State, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Clone)]
    struct Scenario {
        calls: Arc<AtomicUsize>,
        mode: &'static str,
    }
    async fn respond(
        State(s): State<Scenario>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let turn = s.calls.fetch_add(1, Ordering::SeqCst);
        let (text, tool) = match (s.mode, turn) {
            ("quiet", 0) => (
                "Let me check today's date and schedule.",
                Some(("skills_list", json!({}))),
            ),
            ("quiet", 1) => (
                "No game today. Finishing quietly.",
                Some(("finish_quietly", json!({}))),
            ),
            ("result", 0) => (
                "Let me check today's date and schedule.",
                Some(("skills_list", json!({}))),
            ),
            ("result", 1) => (
                "Checking the second source.",
                Some(("commands_list", json!({}))),
            ),
            ("result", 2) => (
                "Yankees play today. First pitch: 7:08 PM ET. Lineups are not posted.",
                None,
            ),
            ("create", 0) => (
                "I will save this reusable review.",
                Some((
                    "skill_save",
                    json!({"name":"Domain review","command":"domain-review","description":"Review a domain and user list","parameters":[{"name":"domain"},{"name":"userlist"}],"body":"Review {{domain}} using {{userlist}}. Return a concise report; do not modify anything."}),
                )),
            ),
            ("create", 1) => ("Saved /domain-review <domain> <userlist>.", None),
            ("create", 2) => {
                assert!(body["messages"].to_string().contains("example.com"));
                assert!(
                    body["messages"]
                        .to_string()
                        .contains("Review {{domain}} using {{userlist}}")
                );
                ("Reviewed example.com using staff.csv.", None)
            }
            _ => panic!("unexpected {} turn {turn}", s.mode),
        };
        let mut message = json!({"role":"assistant","content":text});
        let finish = if let Some((name, args)) = tool {
            message["tool_calls"] = json!([{"id":format!("call_{turn}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]);
            "tool_calls"
        } else {
            "stop"
        };
        sse_response(json!({"choices":[{"message":message,"finish_reason":finish}]}))
    }
    #[tokio::test]
    async fn real_sdk_routines_deliver_only_one_final_result_or_nothing() {
        for mode in ["quiet", "result"] {
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, "openrouter");
            let routine = crate::db::Routine {
                id: crate::db::id(),
                bot_id: bot.id.clone(),
                name: "Game check".into(),
                prompt: "Check the schedule and report concisely or stay quiet".into(),
                interval_seconds: 86400,
                next_run: crate::db::now() + 86400,
                enabled: true,
                run_at: None,
                schedule: None,
            };
            app.db.save_routine(&routine).unwrap();
            app.db.run_routine_now(&routine.id).unwrap();
            let run = app.db.claim().unwrap().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
            let calls = Arc::new(AtomicUsize::new(0));
            let server = tokio::spawn(
                axum::serve(
                    listener,
                    Router::new()
                        .route("/chat/completions", post(respond))
                        .with_state(Scenario {
                            calls: calls.clone(),
                            mode,
                        }),
                )
                .into_future(),
            );
            let output = fixture(&app, &bot, &run, "fixture-key", &endpoint)
                .await
                .unwrap();
            app.db.finish(&run.id, "completed", &output, "").unwrap();
            app.db.chat_complete(&run).unwrap();
            let messages = app.db.chat_messages(&run.chat_id).unwrap();
            if mode == "quiet" {
                assert!(output.is_empty());
                assert!(messages.is_empty());
                assert_eq!(calls.load(Ordering::SeqCst), 2);
            } else {
                assert!(output.starts_with("Yankees play today."));
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0]["text"], output);
                assert_eq!(calls.load(Ordering::SeqCst), 3);
            }
            assert!(
                app.db
                    .events(&run.id)
                    .unwrap()
                    .iter()
                    .any(|e| e["kind"] == "assistant" && e["body"]["phase"] == "commentary")
            );
            server.abort();
        }
    }
    #[tokio::test]
    async fn real_sdk_bot_creates_a_parameterized_command_then_executes_its_saved_workflow() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "openrouter");
        app.db
            .queue(
                &bot.id,
                "/new-skill domain review with domain and userlist parameters",
                0,
            )
            .unwrap();
        let run = app.db.claim().unwrap().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/chat/completions", post(respond))
                    .with_state(Scenario {
                        calls: calls.clone(),
                        mode: "create",
                    }),
            )
            .into_future(),
        );
        let output = fixture(&app, &bot, &run, "fixture-key", &endpoint)
            .await
            .unwrap();
        app.db.finish(&run.id, "completed", &output, "").unwrap();
        app.db.chat_complete(&run).unwrap();
        assert!(
            app.db
                .commands()
                .unwrap()
                .iter()
                .any(|c| c.usage == "/domain-review <domain> <userlist>")
        );
        app.db
            .chat_send(&run.chat_id, "/domain-review example.com staff.csv", &[])
            .unwrap();
        let follow = app.db.claim().unwrap().unwrap();
        assert_eq!(
            fixture(&app, &bot, &follow, "fixture-key", &endpoint)
                .await
                .unwrap(),
            "Reviewed example.com using staff.csv."
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        server.abort();
    }
}
