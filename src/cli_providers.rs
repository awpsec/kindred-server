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
pub(crate) const CLAUDE_SESSION_EXPIRED: &str = "Claude could not refresh its sign-in session. Reconnect Claude in Settings → Connections, then continue this task.";

fn expired_claude_session(provider: &str, frame: &Value) -> bool {
    provider == "claude-code"
        && frame["is_error"] == true
        && frame["text"].as_str().is_some_and(|text| {
            text.trim().starts_with("Failed to authenticate: OAuth session expired and could not be refreshed")
        })
}
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
    // Headless authentication needs no display or reservation.
    let selected=if action=="login" && provider=="kimi-code" {
        let identity=crate::web::screen_selection(app,v["bot_id"].as_str().unwrap_or("")).await?;
        ensure!(app.screen_lock(identity.slot).try_lock().is_ok(),"This bot computer is busy. Try again when its task finishes.");
        v["slot"]=json!(identity.slot);
        Some(identity)
    } else {None};
    let _ownership=if let Some(identity)=&selected {
        let guard=app.screen_ownership_lock(identity.slot).read_owned().await;
        ensure!(app.db.screen_identity(&identity.owner,identity.slot)?==*identity,"This computer ownership changed");
        Some(guard)
    } else {None};
    let mut cmd=vm::ssh(&app.config.vm);
    let tail=format!("python3 {HELPER} {action} {provider}");
    if let Some(identity)=&selected {cmd.arg(crate::screen_capacity::command(identity,"dispatch",None,&tail));}
    else {cmd.arg(tail);}
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
// Model silence and native connector execution are different phases. A native
// call gets a fixed one-hour deadline; intervening frames cannot renew it.
fn frame_timeout(ready: bool, request_seconds: u64, oldest_native: Option<Duration>) -> Duration {
    oldest_native.map(|elapsed| Duration::from_secs(3600).saturating_sub(elapsed))
        .unwrap_or_else(|| Duration::from_secs(if ready { request_seconds } else { 60 }))
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
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    run_child(app,bot,run,child).await
}
async fn run_child(app:&App,bot:&Bot,run:&Run,mut child:tokio::process::Child)->Result<String> {
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let tools = runtime::tool_specs_for_run(app, bot, run);
    let allowed: HashSet<_> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect();
    crate::pi::send(&mut input,json!({"type":"start","protocol":1,"followup_protocol":1,"provider":bot.provider,"model":bot.model,"reasoning_effort":bot.reasoning_effort,
        "instructions":runtime::instructions_for(app,bot,run,&tools,None)?,"prompt":run.prompt,"tools":tools,"max_steps":app.config.max_steps})).await?;
    let mut followup_offer:Option<String>=None;
    let mut initial_boundary=false;
    let mut ready = false;
    let mut text = String::new();
    let mut final_reply = String::new();
    let mut calls = HashSet::new();
    let mut connector_tools: HashMap<String, String> = HashMap::new();
    let mut connector_rows: Vec<Value> = Vec::new();
    let mut connector_account = String::new();
    let mut connector_calls: HashMap<String, Value> = HashMap::new();
    let mut connector_started: HashMap<String, tokio::time::Instant> = HashMap::new();
    loop {
        let native_elapsed = connector_started.values().map(|at| at.elapsed()).max();
        // timeout(0, ready_future) can still return a ready frame. Check the
        // deadline explicitly so a busy stream cannot keep an overdue call alive.
        ensure!(native_elapsed.is_none_or(|elapsed| elapsed < Duration::from_secs(3600)),
            "A native connector call exceeded its execution limit. Its external outcome is unknown; check it before continuing.");
        let frame = tokio::time::timeout(
            frame_timeout(ready, app.config.request_timeout_seconds, native_elapsed),
            crate::pi::read(&mut output),
        )
        .await
        .context(if native_elapsed.is_some() {
            "A native connector call exceeded its execution limit. Its external outcome is unknown; check it before continuing."
        } else { "Subscription provider timed out" })??;
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
                    // This is an explicit terminal authentication failure, not
                    // silence to wait four minutes for and retry five times.
                    if expired_claude_session(&bot.provider, &frame) {
                        bail!(CLAUDE_SESSION_EXPIRED);
                    }
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
                // The CLI bridge has no native phase field. Its verified complete
                // frame identifies the final reply; until then these are updates.
                app.db.event(&run.id, "assistant", json!({"text":part,"phase":"commentary"}))?;
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
                    // Permission review is a human wait, not connector execution.
                    connector_started.remove(id);
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
                    connector_started.insert(id.into(), tokio::time::Instant::now());
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
                connector_started.remove(id);
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
                crate::provider_retry::check_tool_budget(app, run)?;
                let mut result = if name != "set_task_title" && app.db.user_turn_waiting(run)? {
                    app.db.event(&run.id,"user_update_boundary",json!({"tool":name,"call_id":id,"action_started":false}))?;
                    json!({"failed":true,"text":"This tool was not executed: the user sent a new message. End this conversational turn now without further tools or a completion claim. Kindred will deliver the exact message as the next user turn in this same session. Preserve completed actions and continue only work consistent with that message."})
                } else {
                    runtime::call_tool(app, bot, run, name, frame["args"].clone())
                        .await
                        .unwrap_or_else(|e| json!({"text":e.to_string(),"failed":true}))
                };
                if result["deferred_question"] == true
                    || result["finish_quietly"] == true || result["routine_complete"] == true
                    || result["deferred_process"] == true
                {
                    if let Some(offer)=followup_offer.take(){app.db.defer_followup(run,&offer)?;}
                    return Ok(String::new());
                }
                // A send may arrive while the tool is executing. Preserve its
                // actual result, then ask for the safe boundary immediately.
                if app.db.user_turn_waiting(run)? {
                    result["kindred_turn_control"] = json!({"yield_for_user_message":true,"instruction":"End this conversational turn now. A user update will follow through the user channel; do not start more tools or repeat the completed tool."});
                }
                crate::pi::send(
                    &mut input,
                    json!({"type":"tool_result","id":id,"result":result}),
                )
                .await?;
            }
            "turn_complete" if ready => {
                let input_id=runtime::string(&frame,"input_id")?;
                ensure!(!(input_id=="initial"&&initial_boundary),"Repeated subscription initial completion");initial_boundary=true;
                ensure!(input_id==followup_offer.as_deref().unwrap_or("initial"),"Unmatched subscription follow-up completion");
                crate::progress_updates::finish_message(&app.db,&run.id,&final_reply)?;
                if let Some(offer)=followup_offer.take(){app.db.complete_followup_turn(run,&offer)?;}
                ensure!(!app.account_disabled(),"This account is disabled");
                let offer=app.db.offer_followups(run)?;
                let decision=if let Some(offer)=&offer {json!({"type":"turn_decision","id":format!("turn-{input_id}"),"offer_id":offer["id"],"prompt":crate::conversation_updates::followup_prompt(offer)?})} else {json!({"type":"turn_decision","id":format!("turn-{input_id}"),"finish":true})};
                if let Some(offer)=&offer {app.db.event(&run.id,"followup_parent_completed",json!({"offer_id":offer["id"],"output":final_reply}))?;app.db.dispatch_followup(run,runtime::string(offer,"id")?)?;}
                crate::pi::send(&mut input,decision).await?;
                if let Some(offer)=offer {let id=runtime::string(&offer,"id")?;app.db.followup_transported(run,id)?;followup_offer=Some(id.into());}
            }
            "complete" if ready => {
                ensure!(followup_offer.is_none(),"Follow-up turn did not complete");
                crate::provider_inbox::verify_success(&app.db, run)?;
                ensure!(
                    connector_calls.is_empty(),
                    "Connector execution receipt missing"
                );
                ensure!(
                    frame["output"].as_str() == Some(text.as_str()),
                    "Provider completion did not match its output"
                );
                if text.trim().is_empty() {
                    if let Some(error) = crate::providers::routine_empty_error(app, run)? {
                        bail!(error);
                    }
                }
                ensure!(
                    !text.trim().is_empty() || !calls.is_empty(),
                    "Provider completed without a result"
                );
                crate::progress_updates::finish_message(&app.db, &run.id, &final_reply)?;
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
}

#[cfg(all(test, unix))]
pub(crate) async fn routine_protocol_fixture(app: &App, bot: &Bot, run: &Run, script: &str) -> Result<String> {
    let child = tokio::process::Command::new("python3")
        .args(["-u", "-c", script])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
        .kill_on_drop(true).spawn()?;
    run_child(app, bot, run, child).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn waiting_correction_stops_new_tools_and_is_delivered_once_in_same_session() {
        for provider in ["claude-code","kimi-code"] {
            let app=crate::tests::app();let bot=crate::tests::bot(&app.db,provider);
            app.db.queue(&bot.id,"Old assignment",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
            let source=app.db.chat_send(&run.chat_id,"Corrected assignment",&[]).unwrap().remove(0);
            app.db.request_steering(&source,&run.id).unwrap();
            let script=r#"import sys,json
def read():return json.loads(sys.stdin.readline())
def send(v):print(json.dumps(v),flush=True)
s=read();send({'type':'ready','protocol':1,'provider':s['provider'],'model':s['model'],'reasoning_effort':s['reasoning_effort']})
send({'type':'tool_call','id':'stale-action','name':'memory_read','args':{}})
r=read();assert r['result']['failed'] and r['result']['kindred_turn_control']['yield_for_user_message']
assert 'Corrected assignment' not in json.dumps(r)
send({'type':'turn_complete','input_id':'initial'})
d=read();assert 'Corrected assignment' in d['prompt'] and 'Old assignment' not in d['prompt']
send({'type':'tool_call','id':'current-action','name':'memory_read','args':{}})
r=read();assert 'kindred_turn_control' not in r['result']
send({'type':'assistant','text':'Correction handled'})
send({'type':'turn_complete','input_id':d['offer_id']})
assert read()['finish']
send({'type':'complete','output':'Correction handled'})
"#;
            let child=tokio::process::Command::new("python3").args(["-u","-c",script]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true).spawn().unwrap();
            assert_eq!(run_child(&app,&bot,&run,child).await.unwrap(),"Correction handled");
            assert_eq!(app.db.run(&source).unwrap().status,"steered");
            assert!(!app.db.user_turn_waiting(&run).unwrap());
            let events=app.db.events(&run.id).unwrap();
            assert_eq!(events.iter().filter(|e|e["kind"]=="user_followups").count(),1);
            assert_eq!(events.iter().filter(|e|e["kind"]=="tool_started" && e["body"]["tool"]=="memory_read").count(),1);
            assert_eq!(events.iter().filter(|e|e["kind"]=="user_update_boundary").count(),1);
        }
    }
    #[tokio::test]
    async fn classification_subscription_turn_boundary_preserves_each_phase() {
        for provider in ["claude-code","kimi-code"] {for first_work in [false,true] {
            let app=crate::tests::app();let bot=crate::tests::bot(&app.db,provider);app.db.queue(&bot.id,"Synthetic original",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
            let source=app.db.chat_send(&run.chat_id,"Exact synthetic next request",&[]).unwrap()[0].clone();app.db.request_steering(&source,&run.id).unwrap();
            let script=r#"import sys,json
first_work=sys.argv[1]=='true'
def read():return json.loads(sys.stdin.readline())
def send(v):print(json.dumps(v),flush=True)
def tool(i):
 send({'type':'tool_call','id':i,'name':'set_task_title','args':{'title':'Review actual website'}})
 assert read()['type']=='tool_result'
start=read();send({'type':'ready','protocol':1,'provider':start['provider'],'model':start['model'],'reasoning_effort':start['reasoning_effort']})
if first_work:tool('name-first')
send({'type':'assistant','text':'First opening'})
send({'type':'assistant','text':'First final'})
send({'type':'turn_complete','input_id':'initial'})
decision=read();assert 'Exact synthetic next request' in decision['prompt'] and decision['offer_id']
if not first_work:tool('name-second')
send({'type':'assistant','text':'Second opening'})
send({'type':'assistant','text':'Second final'})
send({'type':'turn_complete','input_id':decision['offer_id']})
assert read()['finish']
send({'type':'complete','output':'First opening\nFirst final\nSecond opening\nSecond final'})
"#;
            let child=tokio::process::Command::new("python3").args(["-u","-c",script,if first_work {"true"}else{"false"}]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true).spawn().unwrap();
            assert_eq!(run_child(&app,&bot,&run,child).await.unwrap(),"Second final");
            let rows=app.db.chat_messages(&run.chat_id).unwrap();let find=|text:&str|rows.iter().find(|m|m["text"]==text).unwrap();
            assert_eq!(find("First opening")["progress"]["work_qualified"],first_work);
            assert_eq!(find("Second opening")["progress"]["work_qualified"],!first_work);
            assert_eq!(find("First final")["progress"]["phase"],"final_answer");assert_eq!(find("Second final")["progress"]["phase"],"final_answer");
            assert_eq!(app.db.run(&source).unwrap().status,"steered");
        }}
    }
    #[test]
    fn long_native_calls_have_a_separate_nonrenewing_deadline() {
        assert_eq!(frame_timeout(false, 600, None), Duration::from_secs(60));
        assert_eq!(frame_timeout(true, 600, None), Duration::from_secs(600));
        assert_eq!(frame_timeout(true, 600, Some(Duration::from_secs(300))), Duration::from_secs(3300));
        assert_eq!(frame_timeout(true, 600, Some(Duration::from_secs(3590))), Duration::from_secs(10));
        assert_eq!(frame_timeout(true, 600, Some(Duration::from_secs(3601))), Duration::ZERO);
    }
    #[test]
    fn claude_session_expiry_requires_provider_error_provenance() {
        let text = "Failed to authenticate: OAuth session expired and could not be refreshed";
        assert!(expired_claude_session("claude-code", &json!({"is_error":true,"text":text})));
        assert!(!expired_claude_session("claude-code", &json!({"text":text})));
        assert!(!expired_claude_session("kimi-code", &json!({"is_error":true,"text":text})));
        assert!(!expired_claude_session("claude-code", &json!({"is_error":true,"text":"Connection timed out"})));
    }
}
