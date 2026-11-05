use crate::{
    db::{Bot, Run},
    rpc::Rpc,
    runtime::{self, App},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;

fn codex_tool_result(result: &Value) -> Value {
    let mut items = vec![json!({"type":"inputText","text":result["text"].as_str().unwrap_or("")})];
    if let Some(image) = result["image"].as_str() { items.push(json!({"type":"inputImage","imageUrl":image})); }
    json!({"contentItems":items,"success":result["failed"]!=true})
}

// App-server's `total` is cumulative across model responses and turns; `last`
// describes just one response, not a turn. Keep one updatable receipt per
// invocation, in provider_accounts' existing retry namespace. Summing either
// notifications or `last` would double count repeated delivery and follow-ups.
struct CodexUsage {
    thread: String,
    baseline: Option<[u64; 3]>,
    latest: Option<[u64; 3]>,
    reported: bool,
    current_total_known: bool,
    last_input_tokens: Option<u64>,
}
impl CodexUsage {
    fn new(thread: &str, baseline: Option<[u64; 3]>) -> Self {
        let baseline = baseline.filter(|counts| counts.iter().all(|n| *n <= 1_000_000_000)
            && counts[2] <= counts[0]);
        Self { thread: thread.into(), baseline, latest: baseline, reported: false, current_total_known: false, last_input_tokens: None }
    }

    fn counts(value: &Value) -> Option<[u64; 3]> {
        let count = |key: &str| value[key].as_u64().filter(|n| *n <= 1_000_000_000);
        let counts = [count("inputTokens")?, count("outputTokens")?, count("cachedInputTokens")?];
        (counts[2] <= counts[0]).then_some(counts)
    }

    fn begin_turn(&mut self) { self.reported = false; self.current_total_known = false; self.last_input_tokens = None; }

    fn update(&mut self, params: &Value, current_turn: Option<&str>) -> bool {
        if params["threadId"].as_str() != Some(self.thread.as_str())
            || current_turn.is_none() || params["turnId"].as_str() != current_turn {
            return false;
        }
        let Some(total) = Self::counts(&params["tokenUsage"]["total"]) else {
            // Missing data is unknown, including when a usable `last` exists.
            self.reported = false;
            self.current_total_known = false;
            return true;
        };
        if self.latest.is_some_and(|previous| (0..3).any(|i| total[i] < previous[i])) {
            // Ignore stale/out-of-order snapshots. A counter reset is not a
            // new request and must not cause previously counted work to recur.
            return false;
        }
        self.latest = Some(total);
        self.reported = self.baseline.is_some();
        self.current_total_known = true;
        self.last_input_tokens = params["tokenUsage"]["last"]["inputTokens"].as_u64()
            .filter(|n| *n <= 1_000_000_000);
        true
    }

    fn record(&self, app: &App, bot: &Bot, run: &Run, model: &str) -> Result<()> {
        let counts = match (self.latest, self.baseline) {
            (Some(total), Some(baseline)) => std::array::from_fn(|i| total[i] - baseline[i]),
            _ => [0; 3],
        };
        let mut selected = bot.clone();
        selected.model = model.into();
        crate::provider_accounts::record(app, &selected, run, &json!({
            "request_id":1,"input_tokens":counts[0],"output_tokens":counts[1],
            "cached_tokens":counts[2],"tokens_reported":self.reported,
            "cost":null,"cost_source":"subscription"
        }))
    }
}

fn codex_checkpoint(checkpoint: Option<&Value>) -> Option<(&str, Option<[u64; 3]>)> {
    let checkpoint = checkpoint?;
    if checkpoint["schema"] != 1 { return None; }
    let thread = checkpoint["thread_id"].as_str().filter(|id| !id.is_empty() && id.len() <= 256)?;
    let totals = checkpoint["usage_totals"].as_array().filter(|v| v.len() == 3)
        .and_then(|v| Some([v[0].as_u64()?,v[1].as_u64()?,v[2].as_u64()?]));
    let totals = totals.filter(|v| v.iter().all(|n| *n <= 1_000_000_000) && v[2] <= v[0]);
    Some((thread, totals))
}

fn codex_thread_missing(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.starts_with("codex rejected thread/resume:")
        && (message.contains("thread not found") || message.contains("thread was not found")
            || message.contains("no rollout found") || message.contains("unknown thread"))
}

fn checkpoint_codex_context(app: &App, bot: &Bot, run: &Run, prepared: &crate::harness_context::Prepared, usage: &CodexUsage) {
    let checkpoint = json!({"schema":1,"thread_id":usage.thread,
        "usage_totals":if usage.current_total_known {usage.latest} else {None},
        "estimated_tokens":usage.last_input_tokens.unwrap_or(0)});
    // Work already completed. A stale lease or storage failure must not turn
    // success into a retry that could replay external actions.
    if !matches!(crate::harness_context::complete(app,bot,run,prepared,&checkpoint),Ok(true)) {
        let _=app.db.event(&run.id,"harness_checkpoint_not_saved",json!({"provider":"codex"}));
    }
}

// Empty provider output is not a quiet-completion receipt. Keep the underlying
// scheduled-check failure useful, especially when finish_quietly was refused
// afterwards. This only supplies diagnostics; it never records an outcome or
// changes a provider error, successful tool completion, or deferred decision.
pub(crate) fn routine_empty_error(app: &App, run: &Run) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let c = app.db.0.lock().unwrap();
    if !crate::routine_recovery::scheduled(&c,&run.id)? { return Ok(None); }
    let failure_seq = crate::routine_recovery::latest_failure(&c, &run.id)?.unwrap_or(0);
    let failure: Option<(String, String, bool)> = c.query_row(
        "SELECT substr(CASE WHEN e.kind='tool_result' THEN COALESCE(json_extract(e.body,'$.tool'),'tool') ELSE 'the provider' END,1,128),
             CASE WHEN e.kind='tool_result' THEN substr(json_extract(e.body,'$.text'),1,2048) ELSE '' END, e.kind!='tool_result'
         FROM events e WHERE ((e.kind='tool_result' AND e.seq=?2 AND json_extract(e.body,'$.failed')=1)
             OR (e.run_id=?1 AND e.kind IN ('assistant','provider_diagnostic') AND e.seq>?3 AND json_extract(e.body,'$.status_notice')='provider_error'))
         AND json_type(e.body,'$.text')='text' AND length(trim(json_extract(e.body,'$.text')))>0
         AND EXISTS(SELECT 1 FROM routine_runs WHERE run_id=?1)
         AND NOT EXISTS(SELECT 1 FROM routine_completion WHERE run_id=?1)
         ORDER BY CASE WHEN json_extract(e.body,'$.tool') IN ('finish_quietly','routine_complete') THEN 1 ELSE 0 END, e.seq DESC LIMIT 1",
        rusqlite::params![run.id,failure_seq,crate::routine_recovery::boundary(&c,&run.id)?], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    Ok(failure.and_then(|(tool, text, provider_notice)| {
        if provider_notice {
            // CLI notices can contain complete upstream HTML, challenge tokens
            // or other private diagnostics. Leave them in Activity; the public
            // routine result uses the same safe connection/quota guidance as
            // terminal subscription errors, without changing retry policy.
            return Some("This routine ended without a result after the provider reported an error. Check its connection and quota; review Activity for details.".into());
        }
        let tool = runtime::bounded(tool.trim(), 128);
        let text = runtime::bounded(text.trim(), 2048);
        (!text.is_empty()).then(|| format!(
            "This routine ended without a result after {} failed: {text}",
            if tool.is_empty() { "a tool" } else { tool },
        ))
    }))
}

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
    let mut config = json!({"features.shell_tool":false,"features.unified_exec":false,"features.apps":false,"features.multi_agent":false,"features.multi_agent_v2":false,"web_search":"disabled","apps":{"_default":{"enabled":false,"open_world_enabled":false}}});
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
    let tools = crate::tool_catalog::advertised(app, bot, run);
    let effective = rpc
        .request(
            "config/read",
            json!({"cwd":"/workspace","includeLayers":false}),
        )
        .await?;
    let config = codex_reasoner_config(&effective["config"])?;
    let identity = json!({"model":model,"effort":effort,"account":account["account"],"config":config,
        "context_identity_verified":rpc.context_account.is_some(),"context_account":rpc.context_account,
        "vm":app.config.vm,"connection_generation":crate::harness_context::connection_generation(&app.db,"codex")?});
    let authority=crate::harness_native::authority(app,&identity);
    let mut prepared=crate::harness_context::prepare(app,bot,run,&tools,None,"codex-v1",&identity)?;
    let mut saved=codex_checkpoint(prepared.checkpoint.as_ref()).map(|(id,totals)|(id.to_owned(),totals));
    if let Some((id,_))=&saved {
        if !crate::harness_native::touch(app,bot,run,&authority,&prepared.session_id,id).unwrap_or(false) {
            // Unknown ownership or cleanup already claimed this transcript.
            prepared=crate::harness_context::prepare(app,bot,run,&tools,None,"codex-v1",&identity)?;
            saved=None;
        }
    }
    if rpc.context_account.is_some() && crate::harness_native::drain(app,bot,&authority,rpc).await.is_err() {
        let _=app.db.event(&run.id,"harness_cleanup_pending",json!({"provider":"codex"}));
    }
    let mut resumed = false;
    let mut baseline = Some([0;3]);
    let resumed_thread=if let Some((saved_id,saved_totals))=saved {
        match rpc.request_guarded("thread/resume",json!({"threadId":saved_id,"excludeTurns":true,"cwd":"/workspace","model":model,"approvalPolicy":"on-request","sandbox":"read-only","developerInstructions":prepared.instructions,"config":config})).await {
            Ok(thread)=>{
                ensure!(thread.pointer("/thread/id").and_then(Value::as_str)==Some(saved_id.as_str()),"Codex resumed a different thread");
                resumed=true;baseline=saved_totals;Some(thread)
            }
            Err(error) if codex_thread_missing(&error)=>{
                // No model turn or tool dispatch has occurred. Re-prepare the
                // in-flight lease once, discarding the missing native thread.
                prepared=crate::harness_context::prepare(app,bot,run,&tools,None,"codex-v1",&identity)?;
                ensure!(prepared.checkpoint.is_none(),"Missing Codex thread was not invalidated");
                None
            }
            Err(error)=>return Err(error),
        }
    } else {
        if prepared.checkpoint.is_some() {
            prepared=crate::harness_context::prepare(app,bot,run,&tools,None,"codex-v1",&identity)?;
            ensure!(prepared.checkpoint.is_none(),"Invalid Codex checkpoint was not invalidated");
        }
        None
    };
    let persistent=resumed || (prepared.retention_enabled() && crate::harness_native::reserve(app,bot,run,&authority,&prepared.session_id).unwrap_or(false));
    let thread=match resumed_thread {
        Some(thread)=>thread,
        None=>rpc.request_guarded("thread/start",json!({"cwd":"/workspace","ephemeral":!persistent,"approvalPolicy":"on-request","sandbox":"read-only","developerInstructions":prepared.instructions,"dynamicTools":tools,"config":config,"model":model})).await?,
    };
    let thread_id = thread
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("Codex didn't return a thread ID")?
        .to_owned();
    if persistent && !resumed {
        crate::harness_native::register(app,bot,run,&authority,&prepared.session_id,&thread_id)?;
    }
    verify_codex_reasoner_tools(rpc, &thread_id).await?;
    app.db.event(
        &run.id,
        "model_selected",
        json!({"provider":"codex","model":model,"reasoning_effort":effort,"context_resumed":resumed}),
    )?;
    let started=rpc.request(
        "turn/start",
        json!({"threadId":thread_id,"effort":effort,"input":[{"type":"text","text":prepared.input(&run.prompt)}]}),
    )
    .await?;
    let mut current_turn=started.pointer("/turn/id").and_then(Value::as_str).map(str::to_owned);
    let mut usage = CodexUsage::new(&thread_id, baseline);
    usage.record(app, bot, run, &model)?;
    let mut followup_offer:Option<(String,String)>=None;
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
                    crate::provider_retry::check_tool_budget(app, run)?;
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
                        || result["finish_quietly"] == true || result["routine_complete"] == true
                        || result["deferred_process"] == true;
                    codex_tool_result(&result)
                }
                // All mutation should use the common guest tool policy. Don't silently approve native execution.
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    json!({"decision":"decline"})
                }
                "item/tool/requestUserInput" => {
                    // Native questions bypass Kindred's dynamic-tool dispatcher.
                    // Keep autonomous checks out of human-wait state, but let the
                    // model finish with a useful proposed step in its final report.
                    let initiative = crate::initiative::is_run(&app.db.0.lock().unwrap(), &run.id)?;
                    if initiative {
                        rpc.send(json!({"id":frame["id"],"error":{"code":-32000,"message":"Background initiative cannot ask questions or request new permission. Continue only work already permitted, then report the proposed step for the owner to request directly. If nothing useful remains, use finish_quietly. Do not retry this question or request sensitive information."}})).await?;
                        continue;
                    }
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
                    // this native turn with an empty answer or replay a saved decision.
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
            // Close the RPC while its tool call is blocked: do not send
            // an empty answer that could start another model step before shutdown.
            if end_turn {
                if let Some((offer,_))=followup_offer.take(){app.db.defer_followup(run,&offer)?;}
                return Ok(String::new());
            }
            if method=="item/tool/call"&&followup_offer.is_none() {
                if let Some(turn)=current_turn.as_deref() {
                    ensure!(!app.account_disabled(),"This account is disabled");
                    if let Some(offer)=app.db.offer_followups(run)? {
                        let id=runtime::string(&offer,"id")?;app.db.dispatch_followup(run,id)?;
                        let accepted=rpc.request("turn/steer",json!({"threadId":thread_id,"expectedTurnId":turn,"input":[{"type":"text","text":crate::conversation_updates::followup_prompt(&offer)?}]})).await?;
                        ensure!(accepted["turnId"].as_str()==Some(turn),"Codex steering changed the active turn");
                        app.db.followup_transported(run,id)?;followup_offer=Some((id.into(),turn.into()));output.clear();
                    }
                }
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
            "thread/tokenUsage/updated" => {
                if usage.update(p, current_turn.as_deref()) {
                    usage.record(app, bot, run, &model)?;
                }
            }
            "item/completed"
                if p.pointer("/item/type").and_then(Value::as_str) == Some("agentMessage") =>
            {
                if let Some((_,turn))=&followup_offer {if p["turnId"].as_str()!=Some(turn.as_str()){continue;}}
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
                if status == "failed" && p["turn"]["error"].is_object() {
                    return Err(crate::rpc::CodexTurnError::from_value(&p["turn"]["error"]).into());
                }
                ensure!(
                    status == "completed",
                    "Codex turn ended with status {status}"
                );
                if let Some((offer,turn))=followup_offer.take(){
                    ensure!(p.pointer("/turn/id").and_then(Value::as_str)==Some(turn.as_str()),"Unmatched Codex follow-up completion");
                    app.db.complete_followup_turn(run,&offer)?;
                }
                crate::progress_updates::finish_message(&app.db,&run.id,&output)?;
                ensure!(!app.account_disabled(),"This account is disabled");
                if let Some(offer)=app.db.offer_followups(run)? {
                    app.db.event(&run.id,"followup_parent_completed",json!({"offer_id":offer["id"],"output":output}))?;
                    app.db.dispatch_followup(run,runtime::string(&offer,"id")?)?;
                    let next=rpc.request("turn/start",json!({"threadId":thread_id,"effort":effort,"input":[{"type":"text","text":crate::conversation_updates::followup_prompt(&offer)?}]})).await?;
                    let turn=next.pointer("/turn/id").and_then(Value::as_str).context("Codex follow-up turn identity missing")?;
                    let id=runtime::string(&offer,"id")?;app.db.followup_transported(run,id)?;current_turn=Some(turn.into());followup_offer=Some((id.into(),turn.into()));output.clear();usage.begin_turn();usage.record(app,bot,run,&model)?;continue;
                }
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
                    let next=rpc.request("turn/start",json!({"threadId":thread_id,"effort":effort,"input":[{"type":"text","text":format!("The human subtask has returned control. Respect its tool result: a skipped step is incomplete and must never be treated as successful. Your previous reply did not inspect the resulting page. Continue this same task by taking a fresh computer_screenshot, then continue the unfinished original task. Do not repeat form submission, verification clicks, account creation, or other completed actions. If the screenshot cannot be obtained, state that verification is incomplete. {}",crate::user_tasks::continuation_context(run))}]})).await?;
                    current_turn=next.pointer("/turn/id").and_then(Value::as_str).map(str::to_owned);
                    usage.begin_turn();usage.record(app,bot,run,&model)?;
                    continue;
                }
                if output.trim().is_empty() {
                    if let Some(error) = routine_empty_error(app, run)? {
                        return Err(crate::routine_recovery::Unclassified(error).into());
                    }
                    ensure!(app
                            .db
                            .events(&run.id)?
                            .iter()
                            .any(|e| e["kind"] == "tool_result"
                                && e["body"]["tool"] == "react_to_message"
                                && e["body"]["failed"] != true),
                        "Codex completed without a result"
                    );
                }
                if persistent {checkpoint_codex_context(app,bot,run,&prepared,&usage);}
                return Ok(output);
            }
            "error" => {
                if p["willRetry"] != true {
                    return Err(crate::rpc::CodexTurnError::from_value(&p["error"]).into());
                }
            }
            _ => {}
        }
    }
}

#[cfg(all(test, unix))]
#[path = "memory_contract_tests.rs"]
mod memory_contract_tests;

#[cfg(all(test, unix))]
#[path = "provider_routine_tests.rs"]
mod routine_protocol_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_resume_fallback_only_accepts_explicit_missing_thread_errors() {
        for text in ["Codex rejected thread/resume: thread not found", "Codex rejected thread/resume: no rollout found for thread id"] {
            assert!(codex_thread_missing(&anyhow::anyhow!(text)));
        }
        for text in ["Codex request timed out", "Codex rejected thread/resume: forbidden", "Codex rejected turn/start: thread not found", "unknown thread"] {
            assert!(!codex_thread_missing(&anyhow::anyhow!(text)));
        }
        assert!(codex_checkpoint(Some(&json!({"schema":2,"thread_id":"a"}))).is_none());
        assert_eq!(codex_checkpoint(Some(&json!({"schema":1,"thread_id":"a","usage_totals":[1,2,3]}))),Some(("a",None)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_persistent_context_reuses_exact_thread_without_replaying_actions() {
        for mode in ["reuse","missing","scope-reset","account-reset","checkpoint-failure","resume-denied","turn-failed"] {
            let app=crate::tests::app();let mut bot=crate::tests::bot(&app.db,"codex");
            let script=r#"
import json,sys
phase=int(sys.argv[1]);mode=sys.argv[2];started=0;resumed=0;turns=0;native_id='retained-thread'
def send(v):print(json.dumps(v),flush=True)
def done():
 total={'inputTokens':1200 if resumed and not started else 1000,'outputTokens':130 if resumed and not started else 100,'cachedInputTokens':900 if resumed and not started else 800,'reasoningOutputTokens':10,'totalTokens':1330 if resumed and not started else 1100}
 send({'method':'thread/tokenUsage/updated','params':{'threadId':native_id,'turnId':'current-turn','tokenUsage':{'total':total,'last':{'inputTokens':200,'outputTokens':30,'cachedInputTokens':100,'reasoningOutputTokens':10,'totalTokens':230}}}})
 send({'method':'item/completed','params':{'turnId':'current-turn','item':{'type':'agentMessage','phase':'final_answer','text':'Synthetic finished'}}})
 send({'method':'turn/completed','params':{'threadId':native_id,'turn':{'id':'current-turn','status':'completed'}}})
for line in sys.stdin:
 v=json.loads(line);m=v.get('method');p=v.get('params',{})
 if m=='initialized':continue
 if m=='initialize':result={}
 elif m=='account/read':result={'account':{'type':'chatgpt','email':'other@example.invalid' if phase==2 and mode=='account-reset' else 'fixture@example.invalid'}}
 elif m=='model/list':result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
 elif m=='config/read':result={'config':{}}
 elif m=='thread/delete':
  assert phase==2 and p['threadId']=='retained-thread'
  result={}
 elif m=='thread/resume':
  resumed+=1;assert phase==2 and resumed==1 and started==0 and mode not in ('scope-reset','account-reset','checkpoint-failure')
  assert p['threadId']=='retained-thread' and 'dynamicTools' not in p and 'history' not in p
  assert 'Current Kindred task context' not in p['developerInstructions']
  assert p['config']['features.apps']==False
  if mode in ('missing','resume-denied'):
   send({'id':v['id'],'error':{'code':-32000,'message':'thread not found' if mode=='missing' else 'unauthorized'}});continue
  result={'thread':{'id':'retained-thread'}}
 elif m=='thread/start':
  started+=1;assert started==1 and turns==0
  if phase==2:native_id='replacement-thread'
  assert phase==1 or mode in ('missing','scope-reset','account-reset','checkpoint-failure')
  assert p['ephemeral']==False and 'dynamicTools' in p and 'history' not in p
  assert 'Current Kindred task context' not in p['developerInstructions']
  result={'thread':{'id':native_id}}
 elif m=='app/installed':result={'apps':[]}
 elif m=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
 elif m=='turn/start':
  turns+=1;assert turns==1 and p['threadId']==native_id
  assert p['input'][0]['text'].endswith('# Current user input\n'+('FIRST_ACTION' if phase==1 else 'SECOND_QUESTION'))
  send({'id':v['id'],'result':{'turn':{'id':'current-turn'}}})
  if phase==2 and mode=='turn-failed':
   send({'method':'turn/completed','params':{'threadId':native_id,'turn':{'id':'current-turn','status':'failed','error':{'message':'synthetic refusal','codexErrorInfo':'badRequest'}}}});continue
  if phase==1:send({'id':'once','method':'item/tool/call','params':{'tool':'checklist_create','arguments':{'key':'synthetic-once','title':'Synthetic action performed once','items':[{'title':'Review synthetic evidence','owner':'bot'}]}}})
  else:done()
  continue
 elif v.get('id')=='once':
  assert phase==1 and v['result']['success'];done();continue
 else:raise AssertionError('Unexpected frame: '+str(v))
 send({'id':v['id'],'result':result})
"#;
            if mode=="checkpoint-failure" {
                app.db.0.lock().unwrap().execute_batch("CREATE TRIGGER fail_context_write BEFORE UPDATE OF checkpoint ON harness_contexts WHEN NEW.checkpoint IS NOT NULL BEGIN SELECT RAISE(FAIL,'synthetic checkpoint unavailable'); END;").unwrap();
            }
            let mut run_ids=Vec::new();
            for phase in [1,2] {
                if phase==2 && mode=="scope-reset" {bot.instructions.push_str(" New scope.");app.db.save_bot(&bot).unwrap();}
                app.db.queue(&bot.id,if phase==1 {"FIRST_ACTION"} else {"SECOND_QUESTION"},0).unwrap();
                let run=app.db.claim_bot(&bot.id).unwrap().unwrap();run_ids.push(run.id.clone());
                let mut command=tokio::process::Command::new("python3");command.args(["-u","-c",script,&phase.to_string(),mode]);let mut rpc=Rpc::spawn(command).await.unwrap();
                let result=tokio::time::timeout(Duration::from_secs(10),codex_rpc(&app,&bot,&run,&mut rpc)).await.unwrap();drop(rpc);
                if phase==2 && matches!(mode,"resume-denied"|"turn-failed") {
                    assert!(result.is_err(),"{mode}");
                    let checkpoint:Option<String>=app.db.0.lock().unwrap().query_row("SELECT checkpoint FROM harness_contexts WHERE bot_id=?",[&bot.id],|r|r.get(0)).unwrap();assert!(checkpoint.is_none());
                } else {
                    let output=result.unwrap();assert_eq!(output,"Synthetic finished","{mode}");app.db.finish(&run.id,"completed",&output,"").unwrap();
                    let checkpoint:Option<String>=app.db.0.lock().unwrap().query_row("SELECT checkpoint FROM harness_contexts WHERE bot_id=?",[&bot.id],|r|r.get(0)).unwrap();
                    if mode=="checkpoint-failure" {assert!(checkpoint.is_none());}
                    else {let checkpoint:Value=serde_json::from_str(&checkpoint.expect(mode)).unwrap();assert_eq!(checkpoint["thread_id"],if phase==1 || mode=="reuse" {"retained-thread"} else {"replacement-thread"});assert_eq!(checkpoint["estimated_tokens"],200);}
                }
            }
            let executed:usize=run_ids.iter().map(|id|app.db.events(id).unwrap().iter().filter(|e|e["kind"]=="tool_result"&&e["body"]["tool"]=="checklist_create").count()).sum();
            assert_eq!(executed,1,"{mode}: prior action replayed");
            let lists:i64=app.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM checklists WHERE bot_id=?",[&bot.id],|r|r.get(0)).unwrap();
            assert_eq!(lists,1,"{mode}: checklist was not created exactly once");
            if mode=="reuse" {
                let counts:(i64,i64,i64)=app.db.0.lock().unwrap().query_row("SELECT input_tokens,output_tokens,cached_tokens FROM provider_usage WHERE run_id=?",[&run_ids[1]],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();assert_eq!(counts,(200,30,100));
            }
        }
    }

    fn codex_usage_frame(thread: &str, turn: &str, input: u64, output: u64, cached: u64) -> Value {
        json!({"threadId":thread,"turnId":turn,"tokenUsage":{
            "total":{"inputTokens":input,"outputTokens":output,"cachedInputTokens":cached,
                "reasoningOutputTokens":output / 2,"totalTokens":input + output},
            "last":{"inputTokens":10,"outputTokens":2,"cachedInputTokens":5,
                "reasoningOutputTokens":1,"totalTokens":12}
        }})
    }

    #[test]
    fn codex_usage_cumulative_snapshots_followups_and_retries_do_not_double_count() {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
        app.db.queue(&bot.id,"Synthetic usage",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        let mut usage=CodexUsage::new("thread",Some([0;3]));usage.record(&app,&bot,&run,"selected-model").unwrap();
        for (turn,input,output,cached) in [("one",100,20,80),("one",100,20,80),("one",200,50,140),("two",250,60,180),("two",250,60,180)] {
            usage.begin_turn();
            assert!(usage.update(&codex_usage_frame("thread",turn,input,output,cached),Some(turn)));
            usage.record(&app,&bot,&run,"selected-model").unwrap();
        }
        assert!(!usage.update(&codex_usage_frame("thread","two",200,50,140),Some("two")));
        assert!(!usage.update(&codex_usage_frame("other-thread","two",999,999,0),Some("two")));
        assert!(!usage.update(&codex_usage_frame("thread","one",999,999,0),Some("two")));
        usage.record(&app,&bot,&run,"selected-model").unwrap();
        let c=app.db.0.lock().unwrap();
        let receipt:(i64,i64,i64,i64,i64,Option<f64>,String,String)=c.query_row(
            "SELECT COUNT(*),SUM(input_tokens),SUM(output_tokens),SUM(cached_tokens),MIN(tokens_reported),cost,cost_source,model FROM provider_usage WHERE run_id=?",
            [&run.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).unwrap();
        assert_eq!(receipt,(1,250,60,180,1,None,"subscription".into(),"selected-model".into()));
        drop(c);
        app.db.event(&run.id,"provider_retry",json!({"attempt":1})).unwrap();
        let mut retry=CodexUsage::new("new-thread",Some([0;3]));
        assert!(retry.update(&codex_usage_frame("new-thread","retry",40,10,20),Some("retry")));
        retry.record(&app,&bot,&run,"selected-model").unwrap();
        let counts:(i64,i64,i64,i64)=app.db.0.lock().unwrap().query_row(
            "SELECT COUNT(*),SUM(input_tokens),SUM(output_tokens),SUM(cached_tokens) FROM provider_usage WHERE run_id=?",
            [&run.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(counts,(2,290,70,200));
    }

    #[test]
    fn codex_usage_missing_invalid_and_unverified_resume_baselines_stay_unknown() {
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
        app.db.queue(&bot.id,"Synthetic unknown usage",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        let mut usage=CodexUsage::new("thread",None);
        assert!(usage.update(&codex_usage_frame("thread","one",900,90,800),Some("one")));
        usage.record(&app,&bot,&run,"test/model").unwrap();
        let receipt:(i64,i64,Option<f64>,String)=app.db.0.lock().unwrap().query_row(
            "SELECT input_tokens,tokens_reported,cost,cost_source FROM provider_usage WHERE run_id=?",
            [&run.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(receipt,(0,0,None,"subscription".into()));
        let mut usage=CodexUsage::new("thread",Some([0;3]));
        for bad in [Value::Null,json!({"inputTokens":1,"outputTokens":2}),json!({"inputTokens":-1,"outputTokens":2,"cachedInputTokens":0}),json!({"inputTokens":1,"outputTokens":2,"cachedInputTokens":2}),json!({"inputTokens":1_000_000_001u64,"outputTokens":2,"cachedInputTokens":0})] {
            let mut frame=codex_usage_frame("thread","one",100,20,80);frame["tokenUsage"]["total"]=bad;
            assert!(usage.update(&frame,Some("one")));assert!(!usage.reported);assert_eq!(usage.latest,Some([0;3]));
        }
        assert!(!usage.update(&codex_usage_frame("thread","one",100,20,80),None));
        assert!(usage.update(&codex_usage_frame("thread","one",0,0,0),Some("one")));assert!(usage.reported);
        usage.begin_turn();assert!(!usage.reported);
    }

    #[test]
    fn codex_usage_resume_subtracts_prior_thread_totals() {
        let mut usage=CodexUsage::new("thread",Some([1000,100,800]));
        assert!(usage.update(&codex_usage_frame("thread","new",1120,130,900),Some("new")));
        assert_eq!(usage.latest,Some([1120,130,900]));assert!(usage.reported);
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
        app.db.queue(&bot.id,"Synthetic resumed usage",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        usage.record(&app,&bot,&run,"test/model").unwrap();
        let counts:(i64,i64,i64)=app.db.0.lock().unwrap().query_row(
            "SELECT input_tokens,output_tokens,cached_tokens FROM provider_usage WHERE run_id=?",
            [&run.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(counts,(120,30,100));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_usage_rpc_records_subscription_totals_and_unknown_notifications() {
        for mode in ["reported","missing","last-only","followup","unverified","capped"] {
            let script=r#"
import json,sys
mode=sys.argv[1];turn=0
def send(v):print(json.dumps(v),flush=True)
for line in sys.stdin:
 v=json.loads(line);m=v.get('method')
 if m=='initialized':continue
 if m=='initialize':result={}
 elif m=='account/read':result={'account':{'type':'chatgpt'}}
 elif m=='model/list':result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
 elif m=='config/read':result={'config':{}}
 elif m=='thread/start':
  assert v['params']['ephemeral']==(mode in ('unverified','capped'))
  result={'thread':{'id':'usage-thread'}}
 elif m=='app/installed':result={'apps':[]}
 elif m=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
 elif m=='turn/start':
  turn+=1;assert turn<=2
  tid='turn-'+str(turn)
  send({'id':v['id'],'result':{'turn':{'id':tid}}})
  if mode!='missing':
   usage={'last':{'inputTokens':10,'outputTokens':2,'cachedInputTokens':5,'reasoningOutputTokens':1,'totalTokens':12}}
   if mode!='last-only':usage['total']={'inputTokens':100*turn,'outputTokens':20*turn,'cachedInputTokens':80*turn,'reasoningOutputTokens':10*turn,'totalTokens':120*turn}
   frame={'method':'thread/tokenUsage/updated','params':{'threadId':'usage-thread','turnId':tid,'tokenUsage':usage}}
   send(frame);send(frame)
  send({'method':'item/completed','params':{'turnId':tid,'item':{'type':'agentMessage','text':'Usage checked'}}})
  send({'method':'turn/completed','params':{'threadId':'usage-thread','turn':{'id':tid,'status':'completed'}}})
  continue
 else:raise AssertionError('Unexpected frame: '+str(v))
 send({'id':v['id'],'result':result})
"#;
            let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");
            app.db.queue(&bot.id,"Synthetic usage RPC",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
            if mode=="followup" {let source=app.db.chat_send(&run.chat_id,"Synthetic followup",&[]).unwrap()[0].clone();app.db.request_steering(&source,&run.id).unwrap();}
            if mode=="capped" {for i in 0..128 {assert!(crate::harness_native::reserve(&app,&bot,&run,"fixture-authority",&format!("pending-slot-{i}")).unwrap());}}
            let mut command=tokio::process::Command::new("python3");command.args(["-u","-c",script,mode]);let mut rpc=Rpc::spawn(command).await.unwrap();
            if mode=="unverified" {rpc.context_account=None;}
            let result=tokio::time::timeout(Duration::from_secs(5),codex_rpc(&app,&bot,&run,&mut rpc)).await.unwrap().unwrap();
            assert_eq!(result,"Usage checked");drop(rpc);
            let receipt:(i64,i64,i64,i64,i64,Option<f64>,String)=app.db.0.lock().unwrap().query_row(
                "SELECT COUNT(*),SUM(input_tokens),SUM(output_tokens),SUM(cached_tokens),MIN(tokens_reported),cost,cost_source FROM provider_usage WHERE run_id=?",
                [&run.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap();
            let turns=if mode=="followup" {2} else if matches!(mode,"reported"|"unverified"|"capped") {1} else {0};
            if matches!(mode,"unverified"|"capped") {let saved:i64=app.db.0.lock().unwrap().query_row("SELECT COUNT(*) FROM harness_contexts WHERE bot_id=? AND checkpoint IS NOT NULL",[&bot.id],|r|r.get(0)).unwrap();assert_eq!(saved,0);}
            assert_eq!(receipt,(1,100*turns,20*turns,80*turns,if turns>0 {1} else {0},None,"subscription".into()),"{mode}");
        }
    }
    #[cfg(unix)]
    async fn initiative_codex_protocol(mode: &str) {
        let script = r#"
import json,sys
mode=sys.argv[1]
def send(value):print(json.dumps(value),flush=True)
def checkpoint():send({'id':'describe','method':'item/tool/call','params':{'tool':'tool_describe','arguments':{'names':['initiative_checkpoint']}}})
for line in sys.stdin:
 value=json.loads(line);method=value.get('method')
 if method=='initialized':continue
 if method=='initialize':result={}
 elif method=='account/read':result={'account':{'type':'chatgpt'}}
 elif method=='model/list':result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
 elif method=='config/read':result={'config':{}}
 elif method=='thread/start':
  names=[tool['name'] for tool in value['params']['dynamicTools']]
  assert 'tool_describe' in names and 'tool_execute' in names and 'ask_question' not in names and 'request_user_action' not in names
  result={'thread':{'id':'initiative-thread'}}
 elif method=='app/installed':result={'apps':[]}
 elif method=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
 elif method=='turn/start':
  send({'id':value['id'],'result':{'turn':{'id':'initiative-turn'}}})
  send({'method':'item/completed','params':{'item':{'type':'agentMessage','phase':'commentary','text':'PRIVATE_SCRATCH_ONLY'}}})
  if mode in ('question','secret'):
   params={'questions':[{'question':'May I send this contract?','options':[{'label':'Send'},{'label':'Keep draft'}]}]} if mode=='question' else {'questions':[{'isSecret':True,'question':'PRIVATE_SECRET_QUESTION'}]}
   send({'id':'native-question','method':'item/tool/requestUserInput','params':params})
  else:checkpoint()
  continue
 elif value.get('id')=='native-question':
  assert value['error']['code']==-32000 and 'Background initiative' in value['error']['message']
  assert 'owner' in value['error']['message'] and 'finish_quietly' in value['error']['message']
  checkpoint();continue
 elif value.get('id')=='describe':
  assert value['result']['success'] is True and 'initiative_checkpoint' in json.dumps(value['result'])
  send({'id':'checkpoint','method':'item/tool/call','params':{'tool':'tool_execute','arguments':{'name':'initiative_checkpoint','arguments':{'notes':'Checked synthetic contract; already reported.'}}}})
  continue
 elif value.get('id')=='checkpoint':
  assert value['result']['success'] is True
  if mode=='quiet':
   send({'id':'quiet','method':'item/tool/call','params':{'tool':'finish_quietly','arguments':{}}})
   continue
  text='Prepared a contract outline for your review.' if mode=='report' else 'Draft prepared. Sending needs your direct request.'
  send({'method':'item/completed','params':{'item':{'type':'agentMessage','phase':'final_answer','text':text}}})
  send({'method':'turn/completed','params':{'turn':{'id':'initiative-turn','status':'completed'}}})
  break
 elif value.get('id')=='quiet':raise AssertionError('Quiet completion must end the provider turn without another model step')
 else:raise AssertionError('Unexpected RPC frame: '+str(value))
 send({'id':value['id'],'result':result})
"#;
        let app = crate::tests::app();
        let mut bot = crate::tests::bot(&app.db, "codex");
        bot.profile.initiative = true;
        app.db.save_bot(&bot).unwrap();
        let id = app.db.queue(&bot.id, "Synthetic background contract check", 0).unwrap();
        app.db.0.lock().unwrap().execute(
            "INSERT INTO initiative_runs(run_id,bot_id) VALUES(?,?)",
            rusqlite::params![id, bot.id],
        ).unwrap();
        let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
        let before = app.db.chat_messages(&run.chat_id).unwrap().len();
        let cursor = app.db.notifications(None).unwrap()["cursor"].as_i64().unwrap();
        let mut command = tokio::process::Command::new("python3");
        command.args(["-u", "-c", script, mode]);
        let mut rpc = Rpc::spawn(command).await.unwrap();
        let output = tokio::time::timeout(Duration::from_secs(5), codex_rpc(&app, &bot, &run, &mut rpc))
            .await.unwrap().unwrap();
        drop(rpc);
        assert!(app.db.pending_questions().unwrap().is_empty(), "{mode}: background question persisted");
        assert!(app.db.run_approvals(&run.id).unwrap().is_empty());
        assert!(app.db.pending_user_task(&run.id).unwrap().is_none());
        assert_eq!(app.db.run(&run.id).unwrap().status, "running");
        assert_eq!(crate::initiative::context(&app.db, &run).unwrap()["notes"], "Checked synthetic contract; already reported.");
        app.db.finish(&run.id, "completed", &output, "").unwrap();
        app.db.event(&run.id, "run_finished", json!({})).unwrap();
        app.db.chat_complete(&run).unwrap();
        app.db.chat_complete(&run).unwrap();
        let messages = app.db.chat_messages(&run.chat_id).unwrap();
        let notifications = app.db.notifications(Some(cursor)).unwrap();
        assert!(!messages.iter().any(|message|message.to_string().contains("PRIVATE_")));
        assert!(!notifications.to_string().contains("PRIVATE_"));
        if mode == "quiet" {
            assert!(output.is_empty());
            assert_eq!(messages.len(), before);
            assert!(notifications["items"].as_array().unwrap().is_empty());
        } else {
            let expected = if mode == "report" { "Prepared a contract outline for your review." } else { "Draft prepared. Sending needs your direct request." };
            assert_eq!(output, expected);
            assert_eq!(messages.iter().filter(|message|message["text"]==expected).count(), 1);
            assert_eq!(notifications["items"].as_array().unwrap().len(), 1);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn initiative_codex_report_is_delivered_once_without_scratch_messages() {
        initiative_codex_protocol("report").await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn initiative_codex_quiet_completion_has_no_messages_or_notifications() {
        initiative_codex_protocol("quiet").await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn initiative_codex_native_questions_are_rejected_and_model_can_report_next_step() {
        for mode in ["question", "secret"] {
            initiative_codex_protocol(mode).await;
        }
    }

    #[test]
    fn native_executor_image_preserves_codex_content_and_applied_result() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let Ok(path) = std::env::var("KINDRED_EXECUTOR_IMAGE_PATH") else { return; };
        let png=std::fs::read(path).unwrap();
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()),1440);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()),900);
        let image=format!("data:image/png;base64,{}",STANDARD.encode(png));
        let result=codex_tool_result(&json!({"text":"Input applied","image":image,"action_applied":true}));
        assert_eq!(result["success"],true);
        assert_eq!(result["contentItems"][1]["type"],"inputImage");
        assert_eq!(result["contentItems"][1]["imageUrl"],image);
        let missing=codex_tool_result(&json!({"text":"Input applied; capture unavailable","action_applied":true,"observation_error":true}));
        assert_eq!(missing["success"],true);
        assert_eq!(missing["contentItems"].as_array().unwrap().len(),1);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_followup_is_a_new_user_turn_and_stale_completion_cannot_consume_it() {
        for stale in [false,true] {
            let script=r#"
import json,sys
stale=sys.argv[1]=='true';started=0;tools=0
def send(v):print(json.dumps(v),flush=True)
for line in sys.stdin:
 v=json.loads(line);m=v.get('method')
 if m=='initialized':continue
 if m=='initialize':result={}
 elif m=='account/read':result={'account':{'type':'chatgpt'}}
 elif m=='model/list':result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
 elif m=='config/read':result={'config':{}}
 elif m=='thread/start':result={'thread':{'id':'same-thread'}}
 elif m=='app/installed':result={'apps':[]}
 elif m=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
 elif m=='turn/start':
  started+=1;assert started<=2 and v['params']['threadId']=='same-thread'
  text=v['params']['input'][0]['text']
  if started==1:
   assert text.endswith('# Current user input\nSynthetic original task')
   send({'id':v['id'],'result':{}})
   send({'id':'tool-1','method':'item/tool/call','params':{'tool':'remember','arguments':{'text':'Completed once'}}});continue
  assert 'Exact synthetic followup' in text and 'Synthetic original task' not in text and 'Other private bot' not in text
  send({'id':v['id'],'result':{'turn':{'id':'followup-turn'}}})
  send({'method':'item/completed','params':{'turnId':'followup-turn','item':{'type':'agentMessage','text':'Followup answer'}}})
  send({'method':'turn/completed','params':{'threadId':'same-thread','turn':{'id':'original-turn' if stale else 'followup-turn','status':'completed'}}});break
 elif v.get('id')=='tool-1':
  tools+=1;assert tools==1 and 'Exact synthetic followup' not in json.dumps(v['result'])
  send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'Original answer'}}})
  send({'method':'turn/completed','params':{'threadId':'same-thread','turn':{'id':'original-turn','status':'completed'}}});continue
 else:raise AssertionError('Unexpected frame')
 send({'id':v['id'],'result':result})
"#;
            let mut command=tokio::process::Command::new("python3");command.args(["-u","-c",script,if stale {"true"} else {"false"}]);let mut rpc=Rpc::spawn(command).await.unwrap();
            let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");app.db.queue(&bot.id,"Synthetic original task",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();let source=app.db.chat_send(&run.chat_id,"Exact synthetic followup",&[]).unwrap()[0].clone();app.db.request_steering(&source,&run.id).unwrap();
            let result=codex_rpc(&app,&bot,&run,&mut rpc).await;
            if stale {assert!(result.unwrap_err().to_string().contains("Unmatched Codex"));assert_eq!(app.db.run(&source).unwrap().status,"interrupted");} else {assert_eq!(result.unwrap(),"Followup answer");assert_eq!(app.db.run(&source).unwrap().status,"steered");}
            assert_eq!(app.db.events(&run.id).unwrap().iter().filter(|e|e["kind"]=="tool_result"&&e["body"]["tool"]=="remember").count(),1);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_native_steer_appends_user_input_to_the_verified_active_turn() {
        let script=r#"
import json,sys
steered=False;started=0
def send(v):print(json.dumps(v),flush=True)
for line in sys.stdin:
 v=json.loads(line);m=v.get('method')
 if m=='initialized':continue
 if m=='initialize':result={}
 elif m=='account/read':result={'account':{'type':'chatgpt'}}
 elif m=='model/list':result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
 elif m=='config/read':result={'config':{}}
 elif m=='thread/start':result={'thread':{'id':'native-thread'}}
 elif m=='app/installed':result={'apps':[]}
 elif m=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
 elif m=='turn/start':
  started+=1;assert started==1 and v['params']['input'][0]['text'].endswith('# Current user input\nOriginal task')
  send({'id':v['id'],'result':{'turn':{'id':'active-turn'}}})
  send({'id':'call-1','method':'item/tool/call','params':{'tool':'remember','arguments':{'text':'Once'}}});continue
 elif m=='turn/steer':
  assert not steered and v['params']['expectedTurnId']=='active-turn' and v['params']['threadId']=='native-thread'
  text=v['params']['input'][0]['text'];assert 'Native exact user note' in text and 'Original task' not in text
  steered=True;result={'turnId':'active-turn'}
 elif v.get('id')=='call-1':
  assert steered and 'Native exact user note' not in json.dumps(v['result'])
  send({'method':'item/completed','params':{'turnId':'active-turn','item':{'type':'agentMessage','text':'Active followup answered'}}})
  send({'method':'turn/completed','params':{'turn':{'id':'active-turn','status':'completed'}}});break
 else:raise AssertionError('Unexpected frame')
 send({'id':v['id'],'result':result})
"#;
        let mut command=tokio::process::Command::new("python3");command.args(["-u","-c",script]);let mut rpc=Rpc::spawn(command).await.unwrap();let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");app.db.queue(&bot.id,"Original task",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();let source=app.db.chat_send(&run.chat_id,"Native exact user note",&[]).unwrap()[0].clone();app.db.request_steering(&source,&run.id).unwrap();assert_eq!(codex_rpc(&app,&bot,&run,&mut rpc).await.unwrap(),"Active followup answered");assert_eq!(app.db.run(&source).unwrap().status,"steered");assert_eq!(app.db.events(&run.id).unwrap().iter().filter(|e|e["kind"]=="tool_result").count(),1);
    }

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
        // Simulate a persisted 30-minute-old handoff without a real account or
        // a half-hour wall-clock test. run_limits tests exercise virtual time.
        app.db.0.lock().unwrap().execute("UPDATE user_tasks SET created=created-1800 WHERE id=?", [&task.id]).unwrap();
        assert_eq!(app.db.run(&run.id).unwrap().status, "awaiting_user");
        assert!(app.db.user_task(&task.id).unwrap().created <= crate::db::now()-1800);
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
                let current=messages[1]["content"][0]["text"].as_str().unwrap();
                assert!(current.starts_with("# Current Kindred task context\n"));
                assert!(current.ends_with("\n\nContinue the original request"));
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
            let observation_marker=std::env::temp_dir().join(format!("kindred-handoff-observed-{}",crate::db::id()));
            let script = r#"
import sys,json,time,os
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
    elif m=='thread/delete': send({'id':v['id'],'result':{}})
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
            text=v['params']['input'][0]['text']
            assert 'intermediate checkpoint' in text and 'Check whether a prior request exists' in text
            assert 'Do not repeat' in text
            send({'id':v['id'],'result':{}})
            send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'Still waiting for the human.'}}})
            send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
            break
        assert v['params']['input'][0]['text'].endswith('# Current user input\nCheck whether a prior request exists; read only, do not submit a new request.')
        send({'id':v['id'],'result':{}})
        send({'method':'item/completed','params':{'item':{'type':'agentMessage','phase':'commentary','text':'I will open the sign-in step.'}}})
        send({'id':'human-rpc','method':'item/tool/call','params':{'tool':'request_user_action','arguments':{'title':'Sign in','instructions':'Use the browser'}}})
    elif v.get('id')=='human-rpc':
        assert v['result']['success']==True
        text=v['result']['contentItems'][0]['text']
        assert 'Done with subtask' in text
        assert 'intermediate checkpoint' in text and 'Check whether a prior request exists' in text
        assert 'Do not repeat' in text
        if not stale:
            deadline=time.monotonic()+3
            while not os.path.exists(sys.argv[2]) and time.monotonic()<deadline: time.sleep(0.005)
            assert os.path.exists(sys.argv[2]), 'synthetic screenshot receipt not persisted'
        send({'method':'item/completed','params':{'item':{'type':'agentMessage','text':'Prior request exists: fixture record R-42. No new request submitted.'}}})
        send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
        if not stale: break
    else: raise AssertionError('Unexpected frame')
"#;
            let mut command = tokio::process::Command::new("python3");
            command.args(["-u", "-c", script, if stale { "true" } else { "false" }]).arg(&observation_marker);
            let mut rpc = Rpc::spawn(command).await.unwrap();
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, "codex");
            app.db
                .queue(&bot.id, "Check whether a prior request exists; read only, do not submit a new request.", 0)
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
                let marker=observation_marker.clone();
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
                            // The fake provider must not finish before its synthetic screen read.
                            std::fs::write(&marker,b"observed").unwrap();
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
                std::fs::remove_file(&observation_marker).unwrap();
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
                assert_eq!(output.trim(), "Prior request exists: fixture record R-42. No new request submitted.");
            }
            assert_eq!(app.db.user_tasks().unwrap().len(), 1);
            assert_eq!(app.db.run(&run.id).unwrap().prompt, run.prompt);
            assert!(app.db.claim_bot(&bot.id).unwrap().is_none());
            assert_eq!(app.db.events(&run.id).unwrap().iter().filter(|e|e["kind"]=="user_action_done").count(),1);
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
    elif m=='thread/delete': send({'id':v['id'],'result':{}})
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
    // Synthetic JSONL app-server only. No Codex binary, model, credentials, or VM.
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_error_protocol_preserves_terminal_reason_and_retry_classification() {
        let script = r#"
import json,sys
mode=sys.argv[1]
def send(v):print(json.dumps(v),flush=True)
for line in sys.stdin:
 v=json.loads(line);m=v.get('method')
 if m=='initialized':continue
 if m=='initialize':result={}
 elif m=='account/read':result={'account':{'type':'chatgpt'}}
 elif m=='model/list':result={'data':[{'model':'test/model','isDefault':True,'defaultReasoningEffort':'medium','supportedReasoningEfforts':[{'reasoningEffort':'medium'}]}],'nextCursor':None}
 elif m=='config/read':result={'config':{}}
 elif m=='thread/start':result={'thread':{'id':'error-test-thread'}}
 elif m=='app/installed':result={'apps':[]}
 elif m=='mcpServerStatus/list':result={'data':[],'nextCursor':None}
 elif m=='turn/start':
  send({'id':v['id'],'result':{'turn':{'id':'error-test-turn'}}})
  e={'message':'Synthetic terminal provider reason'}
  if mode in ('notification_policy','completion_policy','retry_then_success'):e['codexErrorInfo']='cyberPolicy' if mode!='retry_then_success' else 'serverOverloaded'
  elif mode=='notification_unknown':e['codexErrorInfo']='futureUnknownCode'
  elif mode=='notification_transient':e['codexErrorInfo']={'responseStreamDisconnected':{'httpStatusCode':503}}
  elif mode=='completion_auth':e['codexErrorInfo']={'httpConnectionFailed':{'httpStatusCode':401}}
  elif mode=='notification_html':e={'message':'<html>PRIVATE_CHALLENGE_BODY</html>','codexErrorInfo':'other'}
  if mode.startswith('completion_'):
   send({'method':'turn/completed','params':{'threadId':'error-test-thread','turn':{'id':'error-test-turn','status':'failed','error':None if mode=='completion_no_error' else e}}})
  else:
   send({'method':'error','params':{'threadId':'error-test-thread','turnId':'error-test-turn','willRetry':mode=='retry_then_success','error':e}})
  if mode=='retry_then_success':
   send({'method':'item/completed','params':{'threadId':'error-test-thread','turnId':'error-test-turn','item':{'type':'agentMessage','phase':'final_answer','text':'Recovered inside provider'}}})
   send({'method':'turn/completed','params':{'threadId':'error-test-thread','turn':{'id':'error-test-turn','status':'completed','error':None}}})
  break
 else:raise AssertionError('Unexpected request: '+str(v))
 send({'id':v['id'],'result':result})
"#;
        for mode in ["notification_policy", "notification_unknown", "notification_uncoded", "notification_transient", "notification_html", "completion_policy", "completion_auth", "completion_no_error", "retry_then_success"] {
            let app = crate::tests::app();
            let bot = crate::tests::bot(&app.db, "codex");
            app.db.queue(&bot.id, "Synthetic protocol error test", 0).unwrap();
            let run = app.db.claim_bot(&bot.id).unwrap().unwrap();
            let mut command = tokio::process::Command::new("python3");
            command.args(["-u", "-c", script, mode]);
            let mut rpc = Rpc::spawn(command).await.unwrap();
            let result = tokio::time::timeout(Duration::from_secs(5), codex_rpc(&app, &bot, &run, &mut rpc)).await.expect(mode);
            drop(rpc);
            if mode == "retry_then_success" {
                assert_eq!(result.unwrap(), "Recovered inside provider");
                continue;
            }
            let error = result.unwrap_err();
            if mode == "completion_no_error" {
                assert!(error.downcast_ref::<crate::rpc::CodexTurnError>().is_none());
                assert_eq!(error.to_string(), "Codex turn ended with status failed");
            } else {
                let typed = error.downcast_ref::<crate::rpc::CodexTurnError>().expect(mode);
                assert_eq!(typed.retryable, mode == "notification_transient", "{mode}");
                if mode == "notification_html" {
                    assert!(!error.to_string().contains("PRIVATE_CHALLENGE_BODY"));
                    assert!(error.to_string().contains("error page"));
                } else {
                    assert!(error.to_string().contains("Synthetic terminal provider reason"), "{mode}: {error}");
                }
            }
            assert!(!error.to_string().contains(crate::provider_retry::EXHAUSTED));
            assert!(!app.db.events(&run.id).unwrap().iter().any(|e| e["kind"] == "tool_requested"));
        }
    }
}
