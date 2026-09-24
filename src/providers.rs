use crate::{
    db::{Bot, Run},
    rpc::Rpc,
    runtime::{self, App},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;

pub async fn codex_models(rpc: &mut Rpc) -> Result<Vec<Value>> {
    let mut models = Vec::new();
    let mut cursor = Value::Null;
    for _ in 0..20 {
        let page = rpc
            .request(
                "model/list",
                json!({"limit":100,"cursor":cursor,"includeHidden":false}),
            )
            .await?;
        models.extend(
            page["data"]
                .as_array()
                .context("Codex returned no model catalog")?
                .iter()
                .cloned(),
        );
        let next = page["nextCursor"].clone();
        if next.is_null() {
            return Ok(models);
        }
        ensure!(next != cursor, "Codex repeated a model catalog page");
        cursor = next;
    }
    bail!("Codex model catalog exceeded the page limit")
}

pub fn codex_selection(models: &[Value], model: &str, effort: &str) -> Result<(String, String)> {
    let selected = models.iter().find(|m| if model.is_empty() { m["isDefault"] == true } else { m["model"].as_str() == Some(model) })
        .context("This model is not in your Codex account catalog. Choose an available model in the bot's settings.")?;
    let effort = if effort.is_empty() {
        selected["defaultReasoningEffort"].as_str().unwrap_or("")
    } else {
        effort
    };
    ensure!(
        selected["supportedReasoningEfforts"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|e| e["reasoningEffort"].as_str() == Some(effort))),
        "This thinking level is not supported by the selected model. Update the bot's settings."
    );
    Ok((
        selected["model"]
            .as_str()
            .context("Model catalog entry has no ID")?
            .into(),
        effort.into(),
    ))
}

async fn openrouter_catalog() -> Result<Value> {
    let mut response = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .get("https://openrouter.ai/api/v1/models")
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(
            anyhow::anyhow!("OpenRouter's model catalog is unavailable; try again.").into(),
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Model catalog exceeded the size limit").into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn openrouter_models() -> std::result::Result<axum::Json<Value>, crate::web::Error> {
    let catalog = openrouter_catalog().await?;
    let data: Vec<Value> = catalog["data"].as_array().into_iter().flatten()
        .filter(|m| m["supported_parameters"].as_array().is_some_and(|p|p.iter().any(|v|v=="tools")))
        .map(|m|json!({"model":m["id"],"displayName":m["name"],"description":m["description"],"reasoning":m["supported_parameters"].as_array().is_some_and(|p|p.iter().any(|v|v=="reasoning")),"isDefault":false})).collect();
    Ok(axum::Json(json!({"data":data})))
}

pub async fn openrouter(app: &App, bot: &Bot, run: &Run) -> Result<String> {
    let key = crate::connections::openrouter_key(app)
        .context("OpenRouter API key is not configured on this server")?;
    let catalog = openrouter_catalog().await?;
    let model = catalog["data"].as_array().into_iter().flatten()
        .find(|model| model["id"] == bot.model)
        .context("The selected model is absent from OpenRouter's current catalog. Update the bot's model selection.")?;
    ensure!(
        model["supported_parameters"]
            .as_array()
            .is_some_and(|p| p.iter().any(|v| v == "tools")),
        "The selected OpenRouter model does not support tools"
    );
    let context = model["context_length"]
        .as_u64()
        .filter(|n| *n >= 1024)
        .context("OpenRouter model catalog has no valid context limit")?;
    let info = json!({
        "id":bot.model, "context_window":context,
        "input_cost":model["pricing"]["prompt"].as_str().and_then(|p|p.parse::<f64>().ok()).map(|n|n*1_000_000.0),
        "output_cost":model["pricing"]["completion"].as_str().and_then(|p|p.parse::<f64>().ok()).map(|n|n*1_000_000.0),
        "max_tokens":model["top_provider"]["max_completion_tokens"],
        "reasoning":model["supported_parameters"].as_array().is_some_and(|p|p.iter().any(|v|v=="reasoning")),
        "vision":model["architecture"]["input_modalities"].as_array().is_some_and(|p|p.iter().any(|v|v=="image"))
    });
    crate::pi::openrouter(app, bot, run, &key, info).await
}

#[cfg(test)]
async fn openrouter_at(
    app: &App,
    bot: &Bot,
    run: &Run,
    key: &str,
    endpoint: &str,
) -> Result<String> {
    crate::pi::fixture(app, bot, run, key, endpoint).await
}

pub async fn codex(app: &App, bot: &Bot, run: &Run) -> Result<String> {
    let mut rpc = Rpc::connect(&app.config.vm).await?;
    codex_rpc(app, bot, run, &mut rpc).await
}
// The reasoner only receives Kindred tools. Native app/MCP execution cannot
// provide Kindred's durable per-call review, so it belongs in the auxiliary
// connector gateway, which never receives a model turn.
fn codex_reasoner_config(effective: &Value) -> Result<Value> {
    ensure!(
        effective.is_object(),
        "Codex effective configuration is unavailable"
    );
    let mut config = json!({"features.shell_tool":false,"features.unified_exec":false,"features.apps":false,"web_search":"disabled","apps":{"_default":{"enabled":false,"open_world_enabled":false}}});
    if let Some(servers) = effective.get("mcp_servers").filter(|v| !v.is_null()) {
        let servers = servers
            .as_object()
            .context("Invalid Codex MCP configuration")?;
        let mut disabled = serde_json::Map::new();
        for name in servers.keys() {
            disabled.insert(name.clone(), json!({"enabled":false}));
        }
        config["mcp_servers"] = json!(disabled);
    }
    Ok(config)
}
async fn verify_codex_reasoner_tools(rpc: &mut Rpc, thread: &str) -> Result<()> {
    let installed = rpc
        .request_guarded(
            "app/installed",
            json!({"threadId":thread,"forceRefresh":false}),
        )
        .await?;
    ensure!(
        installed["apps"]
            .as_array()
            .context("Cannot verify Codex app isolation")?
            .iter()
            .all(|a| a["callable"] == false),
        "Codex still exposes native app tools. This task stopped before its model turn; connector calls must use Kindred review."
    );
    let mut cursor = Value::Null;
    for _ in 0..20 {
        let status = rpc
            .request_guarded(
                "mcpServerStatus/list",
                json!({"threadId":thread,"detail":"full","limit":100,"cursor":cursor}),
            )
            .await?;
        for server in status["data"]
            .as_array()
            .context("Cannot verify Codex MCP isolation")?
        {
            ensure!(
                server["name"] == "codex_apps"
                    || matches!(
                        server["runtimeStatus"].as_str(),
                        Some("disabled" | "notStarted")
                    )
                    || server["tools"].as_object().is_some_and(|t| t.is_empty()),
                "Codex still exposes a native MCP server. This task stopped before its model turn."
            );
        }
        cursor = status["nextCursor"].clone();
        if cursor.is_null() {
            return Ok(());
        }
    }
    bail!("Cannot verify the complete Codex MCP inventory")
}
async fn codex_rpc(app: &App, bot: &Bot, run: &Run, rpc: &mut Rpc) -> Result<String> {
    let account = rpc
        .request("account/read", json!({"refreshToken":false}))
        .await?;
    ensure!(
        account.pointer("/account/type").and_then(Value::as_str) == Some("chatgpt"),
        "Sign in to Codex (Subscription) in Settings > Connections first."
    );
    let models = codex_models(rpc).await?;
    let (model, effort) = codex_selection(&models, &bot.model, &bot.reasoning_effort)?;
    let tools = runtime::tool_specs_for(app, bot);
    let effective = rpc
        .request(
            "config/read",
            json!({"cwd":"/workspace","includeLayers":false}),
        )
        .await?;
    let config = codex_reasoner_config(&effective["config"])?;
    let mut params = json!({"cwd":"/workspace","ephemeral":true,"approvalPolicy":"on-request","sandbox":"read-only","developerInstructions":runtime::instructions_for(app,bot,run,&tools,None)?,"dynamicTools":tools,"config":config});
    params["model"] = json!(model);
    let thread = rpc.request("thread/start", params).await?;
    let thread_id = thread
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("Codex didn't return a thread ID")?
        .to_owned();
    verify_codex_reasoner_tools(rpc, &thread_id).await?;
    app.db.event(
        &run.id,
        "model_selected",
        json!({"provider":"codex","model":model,"reasoning_effort":effort}),
    )?;
    rpc.request(
        "turn/start",
        json!({"threadId":thread_id,"effort":effort,"input":[{"type":"text","text":run.prompt}]}),
    )
    .await?;
    let mut output = String::new();
    let mut output_bytes = 0usize;
    let mut steps = 0usize;
    let mut handoff_recheck = false;
    loop {
        let frame = rpc.next().await?;
        let method = frame["method"].as_str().unwrap_or("");
        let p = &frame["params"];
        if frame.get("id").is_some() && !method.is_empty() {
            let mut end_turn = false;
            let response = match method {
                "item/tool/call" => {
                    steps += 1;
                    let result = if app.config.max_steps > 0 && steps > app.config.max_steps {
                        json!({"text":"Tool budget exhausted; return your partial result.","failed":true})
                    } else {
                        runtime::call_tool(
                            app,
                            bot,
                            run,
                            runtime::string(p, "tool")?,
                            p["arguments"].clone(),
                        )
                        .await
                        .unwrap_or_else(|e| json!({"text":e.to_string(),"failed":true}))
                    };
                    end_turn = result["deferred_question"] == true
                        || result["finish_quietly"] == true
                        || result["deferred_process"] == true;
                    let mut items = vec![
                        json!({"type":"inputText","text":result["text"].as_str().unwrap_or("")}),
                    ];
                    if let Some(image) = result["image"].as_str() {
                        items.push(json!({"type":"inputImage","imageUrl":image}));
                    }
                    json!({"contentItems":items,"success":result["failed"]!=true})
                }
                // All mutation should use the common guest tool policy. Don't silently approve native execution.
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    json!({"decision":"decline"})
                }
                "item/tool/requestUserInput" => {
                    let questions = p["questions"]
                        .as_array()
                        .context("Codex returned no questions")?;
                    ensure!(
                        !questions.is_empty() && questions.len() <= 3,
                        "Codex returned too many questions"
                    );
                    ensure!(
                        questions.iter().all(|q| q["isSecret"] != true),
                        "Sensitive input belongs in a human computer handoff, not a chat question. Use request_user_action."
                    );
                    for question in questions {
                        let text = runtime::string(question, "question")?;
                        let hash = text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                            (h ^ (b as u64)).wrapping_mul(0x100000001b3)
                        });
                        let options = question["options"]
                            .as_array()
                            .context("Use ask_question to supply selectable choices")?
                            .iter()
                            .map(|o| runtime::string(o, "label").map(str::to_string))
                            .collect::<Result<Vec<_>>>()?;
                        app.db.ask_question(
                            run,
                            crate::questions::QuestionInput {
                                topic_key: format!("native:{hash:x}"),
                                question: text.into(),
                                context: if output.trim().is_empty() {
                                    runtime::bounded(&run.prompt, 7000).into()
                                } else {
                                    runtime::bounded(&output, 7000).into()
                                },
                                options,
                            },
                        )?;
                    }
                    // Native input requests use the same persistent cards. Never continue
                    // this ephemeral turn with an empty answer or replay a saved decision.
                    end_turn = true;
                    app.db
                        .event(&run.id, "question_wait", json!({"source":"native_input"}))?;
                    json!({"answers":{}})
                }
                _ => {
                    rpc.send(json!({"id":frame["id"],"error":{"code":-32601,"message":"This client does not support this server request"}})).await?;
                    continue;
                }
            };
            // A persisted decision resumes via a new bounded turn after the answer.
            // Close the ephemeral RPC while its tool call is blocked: do not send
            // an empty answer that could start another model step before shutdown.
            if end_turn {
                return Ok(String::new());
            }
            rpc.send(json!({"id":frame["id"],"result":response}))
                .await?;
            if app.config.max_steps > 0 && steps > app.config.max_steps + 1 {
                bail!(
                    "This task reached its {}-action limit. Completed actions are preserved in Activity. Continue with the remaining steps; do not repeat completed changes.",
                    app.config.max_steps
                );
            }
            continue;
        }
        match method {
            "item/completed"
                if p.pointer("/item/type").and_then(Value::as_str) == Some("agentMessage") =>
            {
                if let Some(text) = p.pointer("/item/text").and_then(Value::as_str) {
                    ensure!(
                        output_bytes + text.len() <= 512 * 1024,
                        "assistant output limit reached"
                    );
                    output_bytes += text.len();
                    // Keep progress in events; the final answer is a separate message.
                    output = text.to_owned();
                    if !app.db.handoff_needs_observation(&run.id)? {
                        app.db.event(
                            &run.id,
                            "assistant",
                            json!({"text":text,"phase":p["item"]["phase"]}),
                        )?;
                    }
                }
            }
            "turn/completed" => {
                let status = p
                    .pointer("/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                ensure!(
                    status == "completed",
                    "Codex turn ended with status {status}"
                );
                // A provider can finish internally while a long human tool call
                // is waiting. Do not publish its queued, pre-handoff conclusion.
                if app.db.handoff_needs_observation(&run.id)? {
                    if handoff_recheck || (app.config.max_steps > 0 && steps >= app.config.max_steps) {
                        return Ok("The human step returned control, but I could not verify the resulting page. The task is incomplete; continue with a fresh screen check before relying on the account or verification state.".into());
                    }
                    handoff_recheck = true;
                    output.clear();
                    app.db.event(
                        &run.id,
                        "handoff_verification_continued",
                        json!({"reason":"missing_post_handoff_observation"}),
                    )?;
                    rpc.request("turn/start",json!({"threadId":thread_id,"effort":effort,"input":[{"type":"text","text":"The human subtask has returned control. Respect its tool result: a skipped step is incomplete and must never be treated as successful. Your previous reply did not inspect the resulting page. Continue this same task by taking a fresh computer_screenshot, then report the observed result. Do not repeat form submission, verification clicks, account creation, or other completed actions. If the screenshot cannot be obtained, state that verification is incomplete."}]})).await?;
                    continue;
                }
                ensure!(
                    !output.trim().is_empty()
                        || app
                            .db
                            .events(&run.id)?
                            .iter()
                            .any(|e| e["kind"] == "tool_result"
                                && e["body"]["tool"] == "react_to_message"
                                && e["body"]["failed"] != true),
                    "Codex completed without a result"
                );
                return Ok(output);
            }
            "error" => {
                if p["willRetry"] != true {
                    bail!(
                        "Codex reported an error: {}",
                        p.pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                    );
                }
            }
            _ => {}
        }
    }
}

#[cfg(all(test, unix))]
#[path = "memory_contract_tests.rs"]
mod memory_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_rejects_native_tool_exposure_and_secret_chat_questions() {
        for mode in ["secret", "apps", "mcp"] {
            let script = r#"
import sys,json
mode=sys.argv[1]
for line in sys.stdin:
 v=json.loads(line);m=v.get('method')
 if m=='initialized': continue
 if m=='initialize': result={}
 elif m=='account/read': result={'account':{'type':'chatgpt'}}
 elif m=='model/list': result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
 elif m=='app/installed': result={'apps':[{'id':'must-not-run','callable':mode=='apps'}]}
 elif m=='mcpServerStatus/list': result={'data':[{'name':'native-escape','runtimeStatus':'connected','tools':{'send':{}}}] if mode=='mcp' else [],'nextCursor':None}
 elif m=='config/read': result={'config':{}}
 elif m=='thread/start': result={'thread':{'id':'secret-fixture'}}
 elif m=='turn/start':
  assert mode=='secret', 'Native tools must block the model turn'
  print(json.dumps({'id':v['id'],'result':{}}),flush=True)
  print(json.dumps({'id':'secret','method':'item/tool/requestUserInput','params':{'questions':[{'id':'public','question':'Continue?','options':[{'label':'Yes'},{'label':'No'}]},{'id':'sensitive','question':'Unlock fixture','isSecret':True,'options':[{'label':'One'},{'label':'Two'}]}]}}),flush=True)
  continue
 else: raise AssertionError('Unexpected call')
 print(json.dumps({'id':v['id'],'result':result}),flush=True)
"#;
            let mut cmd = tokio::process::Command::new("python3");
            cmd.args(["-u", "-c", script, mode]);
            let mut rpc = Rpc::spawn(cmd).await.unwrap();
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, "codex");
            let id = app
                .db
                .queue(&bot.id, "Synthetic native question", 0)
                .unwrap();
            app.db.claim().unwrap();
            let run = app.db.run(&id).unwrap();
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                codex_rpc(&app, &bot, &run, &mut rpc),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(
                error.to_string().contains(if mode == "secret" {
                    "human computer handoff"
                } else {
                    "stopped before its model turn"
                }),
                "{mode}: {error}"
            );
            assert!(
                app.db
                    .decision_context(&bot.id, &run.chat_id, None)
                    .unwrap()
                    .is_empty()
            );
        }
    }
    async fn finish_human(app: &App, run: &Run, slot: i64) {
        let task = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(task) = app.db.pending_user_task(&run.id).unwrap() {
                    break task;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let lock = app.screen_lock(slot);
        let _lease = tokio::time::timeout(Duration::from_secs(3), lock.lock())
            .await
            .unwrap();
        app.db.ready_user_task(&task, slot, "done").unwrap();
    }
    #[tokio::test]
    async fn openrouter_human_subtask_keeps_tool_pairing_and_conversation() {
        use axum::{Json, Router, extract::State, routing::post};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        async fn mock(
            State(calls): State<Arc<AtomicUsize>>,
            Json(body): Json<Value>,
        ) -> axum::response::Response {
            assert_eq!(body["model"], "test/model");
            assert_eq!(body["provider"]["zdr"], true);
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                crate::pi::sse_response(
                    json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"human-call","type":"function","function":{"name":"request_user_action","arguments":"{\"title\":\"Sign in\",\"instructions\":\"Use the browser\"}"}}]},"finish_reason":"tool_calls"}]}),
                )
            } else {
                let messages = body["messages"].as_array().unwrap();
                assert_eq!(
                    messages[1]["content"][0]["text"],
                    "Continue the original request"
                );
                assert_eq!(messages[2]["tool_calls"][0]["id"], "human-call");
                assert_eq!(messages[3]["tool_call_id"], "human-call");
                assert!(
                    messages[3]["content"]
                        .as_str()
                        .unwrap()
                        .contains("Done with subtask")
                );
                crate::pi::sse_response(
                    json!({"choices":[{"message":{"role":"assistant","content":"Original request continued."},"finish_reason":"stop"}]}),
                )
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/chat/completions", post(mock))
                    .with_state(calls.clone()),
            )
            .into_future(),
        );
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "openrouter");
        app.db
            .queue(&bot.id, "Continue the original request", 0)
            .unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let slot = app.db.screen(&bot.id).unwrap();
        let mut lease = Some(app.screen_lock(slot).lock_owned().await);
        let work = runtime::drive_run(
            &app,
            &run,
            slot,
            &mut lease,
            openrouter_at(&app, &bot, &run, "test-key", &endpoint),
        );
        let (result, ()) = tokio::join!(work, finish_human(&app, &run, slot));
        let (output, abrupt) = result.unwrap();
        assert!(!abrupt);
        assert_eq!(output.unwrap().trim(), "Original request continued.");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(app.db.user_tasks().unwrap().len(), 1);
        server.abort();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_human_subtask_returns_to_the_same_rpc_call_and_turn() {
        for stale in [false, true] {
            let script = r#"
import sys,json
def send(v): print(json.dumps(v),flush=True)
started=0
stale=sys.argv[1]=="true"
for line in sys.stdin:
    v=json.loads(line); m=v.get('method')
    if m=='initialize': send({'id':v['id'],'result':{}})
    elif m=='initialized': pass
    elif m=='account/read': send({'id':v['id'],'result':{'account':{'type':'chatgpt'}}})
    elif m=='model/list': send({'id':v['id'],'result':{'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}})
    elif m=='app/installed': send({'id':v['id'],'result':{'apps':[]}})
    elif m=='mcpServerStatus/list': send({'id':v['id'],'result':{'data':[{'name':'fixture_native','runtimeStatus':'disabled','tools':{}}],'nextCursor':None}})
    elif m=='config/read': send({'id':v['id'],'result':{'config':{'mcp_servers':{'fixture_native':{'enabled':True}}},'origins':{}}})
    elif m=='thread/start':
        assert any(t['name']=='request_user_action' for t in v['params']['dynamicTools'])
        assert v['params']['config']['features.apps']==False
        assert v['params']['config']['apps']['_default']['enabled']==False
        assert v['params']['config']['mcp_servers']['fixture_native']['enabled']==False
        assert any(t['name']=='request_user_action' for t in v['params']['dynamicTools'])
        send({'id':v['id'],'result':{'thread':{'id':'same-thread'}}})
    elif m=='turn/start':
        started+=1; assert started<3 and v['params']['threadId']=='same-thread'
        if started==2:
            assert stale and 'fresh computer_screenshot' in v['params']['input'][0]['text']
            send({'id':v['id'],'result':{}})
            send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'Still waiting for the human.'}}})
            send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
            break
        send({'id':v['id'],'result':{}})
        send({'method':'item/completed','params':{'item':{'type':'agentMessage','phase':'commentary','text':'I will open the sign-in step.'}}})
        send({'id':'human-rpc','method':'item/tool/call','params':{'tool':'request_user_action','arguments':{'title':'Sign in','instructions':'Use the browser'}}})
    elif v.get('id')=='human-rpc':
        assert v['result']['success']==True
        assert 'Done with subtask' in v['result']['contentItems'][0]['text']
        send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'Original RPC turn continued.'}}})
        send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
        if not stale: break
    else: raise AssertionError('Unexpected frame')
"#;
            let mut command = tokio::process::Command::new("python3");
            command.args(["-u", "-c", script, if stale { "true" } else { "false" }]);
            let mut rpc = Rpc::spawn(command).await.unwrap();
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, "codex");
            app.db
                .queue(&bot.id, "Continue the original request", 0)
                .unwrap();
            let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
            if stale {
                app.db
                    .event(&run.id, "tool_result", json!({"tool":"computer_click"}))
                    .unwrap();
                app.db
                    .event(
                        &run.id,
                        "tool_result",
                        json!({"tool":"computer_screenshot","has_image":true}),
                    )
                    .unwrap();
            }
            let slot = app.db.screen(&bot.id).unwrap();
            let mut lease = Some(app.screen_lock(slot).lock_owned().await);
            let run_id = run.id.clone();
            let observation = if stale {
                None
            } else {
                let observed = app.clone();
                Some(tokio::spawn(async move {
                    loop {
                        if observed
                            .db
                            .events(&run_id)
                            .unwrap()
                            .iter()
                            .any(|event| event["kind"] == "user_action_done")
                        {
                            observed
                                .db
                                .event(
                                    &run_id,
                                    "tool_result",
                                    json!({"tool":"computer_screenshot","has_image":true,"failed":false}),
                                )
                                .unwrap();
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }))
            };
            let work = runtime::drive_run(
                &app,
                &run,
                slot,
                &mut lease,
                codex_rpc(&app, &bot, &run, &mut rpc),
            );
            let (result, ()) = tokio::join!(work, finish_human(&app, &run, slot));
            let (output, abrupt) = result.unwrap();
            if let Some(observation) = observation {
                observation.await.unwrap();
            }
            assert!(!abrupt);
            let output = output.unwrap();
            if stale {
                assert!(output.contains("could not verify"));
                let events = app.db.events(&run.id).unwrap();
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e["kind"] == "handoff_verification_continued")
                        .count(),
                    1
                );
                assert!(!events.iter().any(|e| e["kind"] == "assistant"
                    && e["body"]["text"] == "Still waiting for the human."));
            } else {
                assert_eq!(output.trim(), "Original RPC turn continued.");
            }
            assert_eq!(app.db.user_tasks().unwrap().len(), 1);
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_choice_cards_stop_before_further_actions_and_resume_in_same_chat() {
        for native in [false, true] {
            let script = r#"
import sys,json
native=sys.argv[1]=='true'
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    v=json.loads(line);m=v.get('method')
    if m=='initialize': send({'id':v['id'],'result':{}})
    elif m=='initialized': pass
    elif m=='account/read': send({'id':v['id'],'result':{'account':{'type':'chatgpt'}}})
    elif m=='model/list': send({'id':v['id'],'result':{'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}})
    elif m=='app/installed': send({'id':v['id'],'result':{'apps':[]}})
    elif m=='mcpServerStatus/list': send({'id':v['id'],'result':{'data':[{'name':'fixture_native','runtimeStatus':'disabled','tools':{}}],'nextCursor':None}})
    elif m=='config/read': send({'id':v['id'],'result':{'config':{'mcp_servers':{'fixture_native':{'enabled':True}}},'origins':{}}})
    elif m=='thread/start':
        assert any(t['name']=='ask_question' for t in v['params']['dynamicTools'])
        assert v['params']['developerInstructions'].startswith('# Kindred operating contract')
        assert 'ask_question' in v['params']['developerInstructions']
        assert any(t['name']=='kindred_guide' for t in v['params']['dynamicTools'])
        assert v['params']['config']['features.apps']==False
        assert v['params']['config']['apps']['_default']['enabled']==False
        assert v['params']['config']['mcp_servers']['fixture_native']['enabled']==False
        send({'id':v['id'],'result':{'thread':{'id':'question-thread'}}})
    elif m=='turn/start':
        send({'id':v['id'],'result':{}})
        prompt=v['params']['input'][0]['text']
        if 'USER\'S RESPONSE: Let it lapse' in prompt:
            assert 'synthetic trial' in prompt
            send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'Understood. I will let it lapse.'}}})
            send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
            break
        send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'A synthetic trial ends Wednesday.'}}})
        if native: send({'id':'choice','method':'item/tool/requestUserInput','params':{'questions':[{'id':'billing','question':'Want to keep it?','options':[{'label':'Add a card'},{'label':'Let it lapse'},{'label':"I'll do it myself"}]}]}})
        else: send({'id':'choice','method':'item/tool/call','params':{'tool':'ask_question','arguments':{'topic_key':'trial-2026-09','question':'Want to keep it?','context':'A synthetic trial ends Wednesday.','options':['Add a card','Let it lapse',"I'll do it myself"]}}})
    elif v.get('id')=='choice':
        # A misbehaving model attempts a later tool. The client must have ended.
        send({'id':'must-not-run','method':'item/tool/call','params':{'tool':'remember','arguments':{'text':'BUG: acted without an answer'}}})
    else: raise AssertionError('Unexpected frame')
"#;
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, "codex");
            app.db
                .queue(&bot.id, "Review a synthetic trial email", 0)
                .unwrap();
            let run = app.db.claim().unwrap().unwrap();
            let mut command = tokio::process::Command::new("python3");
            command.args(["-u", "-c", script, if native { "true" } else { "false" }]);
            let mut rpc = Rpc::spawn(command).await.unwrap();
            let output = tokio::time::timeout(
                Duration::from_secs(5),
                codex_rpc(&app, &bot, &run, &mut rpc),
            )
            .await
            .unwrap()
            .unwrap();
            drop(rpc);
            assert!(
                output.is_empty(),
                "A question ends the turn without replaying its preceding reply"
            );
            assert_eq!(app.db.bot(&bot.id).unwrap().memory, "");
            app.db.finish(&run.id, "completed", &output, "").unwrap();
            app.db.chat_complete(&run).unwrap();
            let id = app
                .db
                .decision_context(&bot.id, &run.chat_id, None)
                .unwrap()[0]["id"]
                .as_str()
                .unwrap()
                .to_string();
            let answered = app
                .db
                .answer_question(
                    &id,
                    crate::questions::Answer {
                        selected: Some(1),
                        custom: None,
                    },
                )
                .unwrap();
            let follow = app.db.claim().unwrap().unwrap();
            assert_eq!(follow.id, answered.continuation_run_id);
            assert_eq!(follow.chat_id, run.chat_id);
            let mut command = tokio::process::Command::new("python3");
            command.args(["-u", "-c", script, if native { "true" } else { "false" }]);
            let mut rpc = Rpc::spawn(command).await.unwrap();
            assert_eq!(
                codex_rpc(&app, &bot, &follow, &mut rpc).await.unwrap(),
                "Understood. I will let it lapse."
            );
        }
    }
    #[test]
    fn codex_uses_selected_model_and_supported_effort_without_fallback() {
        let models = vec![
            json!({"model":"astra","isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"ultra"}]}),
            json!({"model":"spark","isDefault":false,"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}),
        ];
        assert_eq!(
            codex_selection(&models, "spark", "low").unwrap(),
            ("spark".into(), "low".into())
        );
        assert_eq!(
            codex_selection(&models, "spark", "").unwrap(),
            ("spark".into(), "high".into())
        );
        assert_eq!(
            codex_selection(&models, "", "").unwrap(),
            ("astra".into(), "medium".into())
        );
        assert_eq!(
            codex_selection(&models, "", "ultra").unwrap(),
            ("astra".into(), "ultra".into())
        );
        assert!(codex_selection(&models, "spark", "ultra").is_err());
        assert!(codex_selection(&models, "missing", "low").is_err());
    }
    #[tokio::test]
    async fn openrouter_tool_loop_keeps_privacy_headers_and_persists_memory() {
        use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        async fn mock(
            State(calls): State<Arc<AtomicUsize>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> axum::response::Response {
            assert_eq!(headers["x-openrouter-cache"], "false");
            assert_eq!(headers["authorization"], "Bearer test-key");
            assert_eq!(body["provider"]["zdr"], true);
            assert_eq!(body["reasoning"]["effort"], "high");
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                crate::pi::sse_response(
                    json!({"choices":[{"message":{"role":"assistant","content":"Got it, I will remember that.","tool_calls":[{"id":"call1","type":"function","function":{"name":"remember","arguments":"{\"text\":\"Concise reports\"}"}}]},"finish_reason":"tool_calls"}]}),
                )
            } else {
                assert_eq!(body["messages"][3]["tool_call_id"], "call1");
                crate::pi::sse_response(
                    json!({"choices":[{"message":{"role":"assistant","content":"Memory saved."},"finish_reason":"stop"}]}),
                )
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/chat/completions", post(mock))
                    .with_state(calls.clone()),
            )
            .into_future(),
        );
        let app = crate::tests::app();
        let mut bot = crate::tests::bot(&app.db, "openrouter");
        bot.reasoning_effort = "high".into();
        let id = app
            .db
            .queue(&bot.id, "Remember concise reports", 0)
            .unwrap();
        let run = app.db.claim().unwrap().unwrap();
        assert_eq!(
            openrouter_at(&app, &bot, &run, "test-key", &endpoint)
                .await
                .unwrap()
                .trim(),
            "Memory saved."
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(app.db.bot(&bot.id).unwrap().memory, "Concise reports");
        assert!(!app.db.events(&id).unwrap().is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn openrouter_ineligible_endpoint_is_a_visible_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/chat/completions",
                    axum::routing::post(|| async {
                        (
                            axum::http::StatusCode::NOT_FOUND,
                            "private upstream error content",
                        )
                    }),
                ),
            )
            .into_future(),
        );
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "openrouter");
        app.db.queue(&bot.id, "test", 0).unwrap();
        let run = app.db.claim().unwrap().unwrap();
        let error = openrouter_at(&app, &bot, &run, "test-key", &endpoint)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("404"));
        assert!(error.contains("no alternate provider"));
        assert!(!error.contains("private upstream"));
        server.abort();
    }
}
