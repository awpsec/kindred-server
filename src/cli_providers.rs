use crate::{
    db::{Bot, Run},
    runtime::{self, App, Shared},
    vm,
};
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    time::Duration,
};
use tokio::io::BufReader;

const HELPER: &str = "/usr/local/lib/kindred/provider-cli.py";
pub async fn action(
    State(app): State<Shared>,
    Path((provider, action)): Path<(String, String)>,
    Json(mut v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(account(&app, &provider, &action, &mut v).await?))
}
async fn account(app: &Shared, provider: &str, action: &str, v: &mut Value) -> Result<Value> {
    ensure!(
        matches!(provider, "claude-code" | "kimi-code")
            && (matches!(action, "account" | "models" | "usage" | "login" | "logout")
                || (provider == "claude-code"
                    && matches!(
                        action,
                        "login-status" | "login-complete" | "login-cancel" | "connectors"
                    ))),
        "Unsupported provider action"
    );
    if action != "login"
        && let Some(status) = vm::provider_unavailable(&app.config.vm, provider).await?
    {
        return Ok(status);
    }
    if action == "login" {
        vm::ensure_running(&app.config.vm).await?;
    }
    // Claude authentication is profile-wide and headless; it needs no bot display.
    if action == "login" && provider == "kimi-code" {
        let slot = crate::web::screen_slot(app, v["bot_id"].as_str().unwrap_or(""))?;
        ensure!(
            app.screen_lock(slot).try_lock().is_ok(),
            "This bot computer is busy. Try again when its task finishes."
        );
        v["slot"] = json!(slot);
    }
    let mut cmd = vm::ssh(&app.config.vm);
    cmd.arg(format!("python3 {HELPER} {action} {provider}"));
    let result=vm::capture(cmd,Some(serde_json::to_vec(v)?),35,512*1024).await
        .context("The provider CLI is unavailable. Install the pinned Claude Code and Kimi Code runtimes in the bot VM")?;
    let result: Value = serde_json::from_slice(&result)?;
    ensure!(
        result["type"] != "error",
        "The provider could not complete this account action"
    );
    if provider == "claude-code" {
        if action == "connectors" {
            crate::connector_policy::catalogue(
                &app.db,
                runtime::string(&result, "account_key")?,
                &result["data"],
            )?;
        }
        if action == "logout" {
            crate::connector_policy::clear_catalogue(&app.db)?;
        }
    }
    Ok(result)
}
pub async fn run(app: &App, bot: &Bot, run: &Run) -> Result<String> {
    ensure!(
        matches!(bot.provider.as_str(), "claude-code" | "kimi-code"),
        "Unsupported subscription provider"
    );
    ensure!(
        bot.reasoning_effort.is_empty()
            || (bot.provider == "claude-code"
                && matches!(
                    bot.reasoning_effort.as_str(),
                    "low" | "medium" | "high" | "xhigh" | "max"
                )),
        "Choose the provider default thinking level for this subscription"
    );
    let mut cmd = vm::ssh(&app.config.vm);
    cmd.arg(format!("python3 {HELPER} worker {}", bot.provider));
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let tools = runtime::tool_specs_for(app, bot);
    let allowed: HashSet<_> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    crate::pi::send(&mut input,json!({"type":"start","protocol":1,"provider":bot.provider,"model":bot.model,"reasoning_effort":bot.reasoning_effort,
        "instructions":runtime::instructions_for(app,bot,run,&tools,None)?,"prompt":run.prompt,"tools":tools,"max_steps":app.config.max_steps})).await?;
    let mut ready = false;
    let mut text = String::new();
    let mut final_reply = String::new();
    let mut calls = HashSet::new();
    let mut connector_tools: HashMap<String, String> = HashMap::new();
    let mut connector_rows: Vec<Value> = Vec::new();
    let mut connector_account = String::new();
    let mut connector_calls: HashMap<String, Value> = HashMap::new();
    for _ in 0..10000 {
        let frame = tokio::time::timeout(
            Duration::from_secs(if ready { 240 } else { 60 }),
            crate::pi::read(&mut output),
        )
        .await
        .context("Subscription provider timed out")??;
        match frame["type"].as_str().unwrap_or("") {
            "connector_catalogue"
                if !ready && bot.provider == "claude-code" && connector_account.is_empty() =>
            {
                connector_account = runtime::string(&frame, "account_key")?.to_string();
                crate::provider_inbox::verify_catalogue(
                    &app.db,
                    bot,
                    run,
                    &connector_account,
                    &frame["connectors"],
                )?;
                crate::connector_policy::catalogue(
                    &app.db,
                    &connector_account,
                    &frame["connectors"],
                )?;
                connector_rows = frame["connectors"].as_array().unwrap().clone();
                let context = crate::connector_policy::prepare(app, bot, run).await?;
                crate::pi::send(
                    &mut input,
                    json!({"type":"tool_result","id":"connector-context","result":context}),
                )
                .await?;
            }
            "ready" => {
                ensure!(
                    !ready
                        && frame["protocol"] == 1
                        && frame["provider"] == bot.provider
                        && frame["model"] == bot.model
                        && frame["reasoning_effort"].as_str().unwrap_or("") == bot.reasoning_effort,
                    "Unexpected subscription provider handshake"
                );
                ready = true;
                if let Some(rows) = frame["connectors"].as_array() {
                    ensure!(
                        !connector_account.is_empty() && rows == &connector_rows,
                        "Connector catalogue changed during startup"
                    );
                    ensure!(
                        bot.provider == "claude-code" && rows.len() <= 200,
                        "Invalid connector catalogue"
                    );
                    for row in rows {
                        ensure!(
                            row["origin"] == "claude-account",
                            "Invalid connector origin"
                        );
                        let connection = runtime::string(row, "name")?;
                        ensure!(
                            connection.starts_with("claude.ai ") && connection.len() <= 200,
                            "Invalid connector name"
                        );
                        for tool in row["tools"].as_array().context("Missing connector tools")? {
                            let tool = tool.as_str().context("Invalid connector tool")?;
                            ensure!(
                                row["status"] == "connected"
                                    && tool.starts_with("mcp__claude_ai_")
                                    && tool.len() <= 512
                                    && connector_tools
                                        .insert(tool.into(), connection.into())
                                        .is_none(),
                                "Invalid connector tool catalogue"
                            );
                        }
                    }
                    app.db.event(&run.id, "connectors_discovered", json!({"provider":bot.provider,"origin":"claude-account","connections":rows}))?;
                }
                app.db.event(
                    &run.id,
                    "model_selected",
                    json!({"provider":bot.provider,"model":bot.model,"reasoning_effort":bot.reasoning_effort,"harness":"official-cli","tool_bridge_verified":frame["tool_bridge_verified"],"tool_count":frame["tool_count"]}),
                )?;
            }
            "assistant" if ready || frame["is_error"] == true => {
                let part = runtime::string(&frame, "text")?;
                if frame["is_error"] == true {
                    ensure!(part.len() < 512 * 1024, "Provider output limit reached");
                    app.db.event(
                        &run.id,
                        "provider_diagnostic",
                        json!({"text":part,"status_notice":"provider_error"}),
                    )?;
                    continue;
                }
                ensure!(
                    text.len() + part.len() < 512 * 1024,
                    "Provider output limit reached"
                );
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(part);
                final_reply = part.to_owned();
                app.db.event(&run.id, "assistant", json!({"text":part}))?;
            }
            "usage" if ready => crate::provider_accounts::record(app, bot, run, &frame)?,
            "connector_approval" if ready && bot.provider == "claude-code" => {
                ensure!(
                    !app.db.cancelled(&run.id) && !app.db.turn_deferred(&run.id)?,
                    "Run cannot execute tools"
                );
                let id = runtime::string(&frame, "id")?;
                let reply_id = runtime::string(&frame, "reply_id")?;
                let name = runtime::string(&frame, "name")?;
                let connection = connector_tools
                    .get(name)
                    .context("Unverified Claude connector")?;
                let force = frame["force"] == true;
                let row = connector_rows
                    .iter()
                    .find(|r| r["name"] == *connection)
                    .context("Missing connector identity")?;
                let mut args = json!({"origin":"claude-account","account_key":connector_account,"connector_key":row["connector_key"],"forced":force,"connection":connection,"tool_name":name,"input":frame["args"],
                    "approval_reason":if force {"Claude or the connector requires explicit approval for this call."} else {"Kindred bot approval preference"}});
                crate::provider_inbox::guard_tool(&app.db, run, "claude_connector", &args)?;
                ensure!(
                    id.len() <= 512
                        && reply_id
                            == format!(
                                "{}{}",
                                if force { "permission-" } else { "connector-" },
                                id
                            ),
                    "Invalid connector call identity"
                );
                if force {
                    let previous = connector_calls
                        .get(id)
                        .context("Connector permission without a call")?;
                    ensure!(
                        previous["tool_name"] == name
                            && previous["input"] == frame["args"]
                            && previous["forced"] != true,
                        "Changed connector permission request"
                    );
                    args["artifact_id"] = previous["artifact_id"].clone();
                    connector_calls.get_mut(id).unwrap()["forced"] = json!(true);
                } else {
                    ensure!(
                        calls.insert(format!("connector-{id}"))
                            && (app.config.max_steps == 0 || calls.len() <= app.config.max_steps),
                        "Connector step limit"
                    );
                    crate::provider_retry::check_tool_budget(app, run)?;
                    args["artifact_id"] =
                        json!(crate::connector_artifacts::create(&app.db, run, &args)?);
                    connector_calls.insert(id.into(), args.clone());
                    app.db.event(
                        &run.id,
                        "tool_requested",
                        json!({"tool":"claude_connector","args":args,"call_id":id}),
                    )?;
                }
                let decision = crate::connector_artifacts::review(
                    app,
                    bot,
                    run,
                    "claude_connector",
                    &mut args,
                    force,
                )
                .await;
                let (approved, message) = match decision {
                    Ok(allowed) => (allowed, String::new()),
                    Err(error) => (false, error.to_string()),
                };
                connector_calls.insert(id.into(), args.clone());
                ensure!(!app.db.cancelled(&run.id), "Run cancelled");
                if approved {
                    crate::connector_artifacts::dispatch(&app.db, run, &args)?;
                    app.db.event(
                        &run.id,
                        "tool_started",
                        json!({"tool":"claude_connector","args":args,"call_id":id}),
                    )?;
                }
                crate::pi::send(
                    &mut input,
                    json!({"type":"tool_result","id":reply_id,"result":{"approved":approved,"updated_input":args["input"],"message":message}}),
                )
                .await?;
            }
            "connector_result" if ready && bot.provider == "claude-code" => {
                let id = runtime::string(&frame, "id")?;
                let args = connector_calls
                    .remove(id)
                    .context("Unexpected connector execution receipt")?;
                let output = runtime::string(&frame, "text")?;
                ensure!(output.len() <= 1024 * 1024, "Connector output limit");
                app.db.event(&run.id, "tool_result", json!({"tool":"claude_connector","args":args,"call_id":id,"text":output,"failed":frame["failed"] == true}))?;
                let parsed = serde_json::from_str::<Value>(output)
                    .unwrap_or_else(|_| json!({"text":output}));
                crate::connector_artifacts::complete(
                    &app.db,
                    args["artifact_id"]
                        .as_str()
                        .context("Connector card missing")?,
                    &parsed,
                    frame["failed"] == true,
                )?;
            }
            "tool_call" if ready => {
                ensure!(!app.db.cancelled(&run.id), "Run cancelled");
                let id = runtime::string(&frame, "id")?;
                let name = runtime::string(&frame, "name")?;
                ensure!(
                    id.len() <= 512
                        && calls.insert(id.to_owned())
                        && (app.config.max_steps == 0 || calls.len() <= app.config.max_steps)
                        && allowed.contains(name),
                    "Invalid provider tool request"
                );
                let result = runtime::call_tool(app, bot, run, name, frame["args"].clone())
                    .await
                    .unwrap_or_else(|e| json!({"text":e.to_string(),"failed":true}));
                if result["deferred_question"] == true
                    || result["finish_quietly"] == true
                    || result["deferred_process"] == true
                {
                    return Ok(String::new());
                }
                crate::pi::send(
                    &mut input,
                    json!({"type":"tool_result","id":id,"result":result}),
                )
                .await?;
            }
            "complete" if ready => {
                crate::provider_inbox::verify_success(&app.db, run)?;
                ensure!(
                    connector_calls.is_empty(),
                    "Connector execution receipt missing"
                );
                ensure!(
                    frame["output"].as_str() == Some(text.as_str()),
                    "Provider completion did not match its output"
                );
                ensure!(
                    !text.trim().is_empty() || !calls.is_empty(),
                    "Provider completed without a result"
                );
                return Ok(final_reply);
            }
            "error" if frame["code"] == "tool_bridge_unavailable" => bail!(
                "Claude could not connect to Kindred tools. This task did not continue without tools. Retry when the bot computer is available."
            ),
            "error" if frame["code"] == "provider_configuration" => bail!(
                "The selected subscription model or thinking level is unavailable. Check this bot's provider settings."
            ),
            "error" => bail!(
                "The subscription provider could not complete this task. Check its official CLI connection and quota; no alternate provider was used."
            ),
            _ => bail!("Unexpected subscription provider frame"),
        }
    }
    bail!("Provider event limit reached")
}
