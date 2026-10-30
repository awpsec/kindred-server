//! Imported Codex apps use verified runtime bindings and permission-checked, turnless RPCs.
use crate::{
    connector_artifacts,
    db::{Bot, Run},
    rpc::Rpc,
    runtime::{self, App},
};
use anyhow::{Context, Result, bail, ensure};
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn fingerprint(account: &Value) -> Result<String> {
    ensure!(
        account["type"] == "chatgpt"
            && account["email"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
        "Sign in to Codex in Settings > Connections first."
    );
    let identity = json!({"type":account["type"],"email":account["email"]});
    Ok(digest(&SHA256, identity.to_string().as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
#[cfg(all(test, unix))]
#[path = "codex_connector_tests.rs"]
mod tests;
async fn page(rpc: &mut Rpc, method: &str, mut params: Value) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut cursor = Value::Null;
    let mut seen = HashSet::new();
    for _ in 0..20 {
        params["cursor"] = cursor;
        let result = rpc.request_guarded(method, params.clone()).await?;
        let rows = result["data"]
            .as_array()
            .context("Invalid Codex inventory page")?;
        ensure!(
            out.len() + rows.len() <= 2000,
            "Codex inventory limit exceeded"
        );
        out.extend(rows.iter().cloned());
        cursor = result["nextCursor"].clone();
        if cursor.is_null() {
            return Ok(out);
        }
        let next = cursor.as_str().context("Invalid Codex inventory cursor")?;
        ensure!(
            !next.is_empty() && seen.insert(next.to_owned()),
            "Repeated Codex inventory cursor"
        );
    }
    bail!("Codex inventory pagination limit exceeded")
}
async fn auxiliary(rpc: &mut Rpc) -> Result<String> {
    let account = rpc
        .request_guarded("account/read", json!({"refreshToken":false}))
        .await?;
    fingerprint(&account["account"])?;
    // Preserve the person's effective app policy. Never turn on disabled apps.
    let thread=rpc.request_guarded("thread/start",json!({"cwd":"/workspace","ephemeral":true,"sandbox":"read-only","approvalPolicy":"on-request","config":{"features.shell_tool":false,"features.unified_exec":false,"web_search":"disabled"}})).await?;
    Ok(runtime::string(&thread["thread"], "id")?.to_owned())
}
fn connections(metadata: &Value, installed: &Value, servers: &[Value]) -> Result<Vec<Value>> {
    let metas = metadata["apps"]
        .as_array()
        .context("Invalid Codex app metadata")?;
    let installed = installed["apps"]
        .as_array()
        .context("Invalid Codex installed-app snapshot")?;
    // Canonical summary names must resolve to exactly one app. The summaries
    // supply provenance only; installed state and MCP schema supply availability.
    let mut owners: HashMap<&str, HashSet<&str>> = HashMap::new();
    for meta in metas {
        let id = runtime::string(meta, "id")?;
        for t in meta["toolSummaries"].as_array().into_iter().flatten() {
            if let Some(name) = t["name"].as_str() {
                owners.entry(name).or_default().insert(id);
            }
        }
    }
    let hosted: Vec<_> = servers
        .iter()
        .filter(|s| s["name"] == "codex_apps")
        .collect();
    let server = if hosted.len() == 1
        && hosted[0]["runtimeStatus"] == "connected"
        && !matches!(hosted[0]["authStatus"].as_str(), Some("notLoggedIn"))
    {
        Some(hosted[0])
    } else {
        None
    };
    let mut rows = Vec::new();
    let mut ids = HashSet::new();
    for app in installed {
        let id = runtime::string(app, "id")?;
        ensure!(
            !id.is_empty() && id.len() <= 256 && ids.insert(id),
            "Invalid or duplicate Codex app identity"
        );
        let metas_for: Vec<_> = metas.iter().filter(|m| m["id"] == id).collect();
        let name = metas_for
            .first()
            .and_then(|m| m["name"].as_str())
            .or_else(|| app["runtimeName"].as_str())
            .unwrap_or(id);
        // Installed runtime state, not directory/display metadata, authorizes
        // availability. Tool provenance and live schemas are still required.
        let available = app["enabled"] == true && app["callable"] == true;
        let mut tools = Vec::new();
        if available {
            if let Some(server) = server {
                for (tool, schema) in server["tools"].as_object().into_iter().flatten() {
                    if schema["name"] != tool.as_str() || !schema["inputSchema"].is_object() {
                        continue;
                    }
                    let provenance = schema["_meta"]["connector_id"].as_str();
                    let summaries: Vec<_> = metas_for
                        .iter()
                        .flat_map(|m| m["toolSummaries"].as_array().into_iter().flatten())
                        .filter(|t| {
                            t["name"] == tool.as_str()
                                || t["name"].as_str() == tool.rsplit_once('.').map(|(_, name)| name)
                        })
                        .collect();
                    // Display summaries use bare names; the runtime uses qualified
                    // names. Never guess app ownership from a prefix or suffix.
                    let bound = match provenance {
                        Some(owner) => owner == id,
                        None => {
                            metas_for.len() == 1
                                && owners
                                    .get(tool.as_str())
                                    .is_some_and(|o| o.len() == 1 && o.contains(id))
                        }
                    };
                    if !bound || summaries.iter().any(|s| s["isEnabled"] == false) {
                        continue;
                    }
                    let link = schema["_meta"]["link_id"]
                        .as_str()
                        .filter(|s| !s.is_empty());
                    tools.push(json!({"name":tool,"description":schema["description"],"inputSchema":schema["inputSchema"],
                        "read_only":schema["annotations"]["readOnlyHint"]==true && !summaries.iter().any(|s| s["isReadOnly"]==false),
                        "runtime_binding":if provenance == Some(id) {json!({"app_id":id,"link_id":link,"link_selector":schema["inputSchema"]["properties"]["link_id"]})} else {Value::Null}}));
                }
            }
        }
        let executable = !tools.is_empty();
        // Include the runtime's account selector schema: multiplexed apps can
        // expose several accounts behind one tool-level link id. A reconnect or
        // changed account set must not inherit a grant for the previous binding.
        let persistent = executable
            && tools
                .iter()
                .all(|t| t["runtime_binding"]["link_id"].is_string());
        let connector_key = if persistent {
            let mut bindings: Vec<_> = tools
                .iter()
                .map(|t| t["runtime_binding"].to_string())
                .collect();
            bindings.sort();
            bindings.dedup();
            let bytes = json!({"app_id":id,"bindings":bindings}).to_string();
            format!(
                "codex:{}",
                digest(&SHA256, bytes.as_bytes())
                    .as_ref()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        } else {
            id.to_owned()
        };
        rows.push(json!({"origin":"codex-account","name":format!("codex {name}"),"display_name":name,"connector_key":connector_key,"app_id":id,"server":"codex_apps","status":if executable{"connected"}else{"unavailable"},"execution_available":executable,"permission_persistence_available":persistent,"requires_approval":true,"always_allow":false,"linked_account_verified":persistent,"tools":tools.iter().map(|t|t["name"].clone()).collect::<Vec<_>>(),"tool_schemas":tools,"availability_message":if persistent{"Uses this bot's connector permissions. Provider restrictions and email-send reviews still apply."}else if executable{"Each call needs review because Codex did not identify a stable connection."}else if !available{"This app is disabled or not callable in Codex. Check its connection settings."}else{"Codex returned no verified callable tools for this app. Refresh the connection; if it persists, check the bot computer's Codex runtime."}}));
    }
    Ok(rows)
}
// Direct MCP RPCs do not run a model turn's approval machinery. An
// explicitly configured provider policy therefore remains review-only here;
// automatic Kindred permission inheritance is limited to the provider default.
// Keep unavailable/unknown policy snapshots fail-closed for automatic calls.
fn provider_default_policy(config: Option<&Value>, requirements: Option<&Value>) -> bool {
    let Some(config) = config
        .and_then(|v| v.get("config"))
        .filter(|v| v.is_object())
    else {
        return false;
    };
    let Some(requirements) = requirements.filter(|v| v.get("requirements").is_some()) else {
        return false;
    };
    // Kindred's launcher restricts authentication to ChatGPT. Codex reports
    // that as managed requirements even when there is no action policy.
    // fingerprint() independently verifies ChatGPT auth; this login restriction
    // must not turn Full access into per-call review. Keep all other non-null
    // requirements fail-closed, including future fields we do not recognize.
    let requirements = &requirements["requirements"];
    let login_only = requirements.is_null()
        || requirements.as_object().is_some_and(|fields| {
            fields.iter().all(|(key, value)| {
                value.is_null()
                    || (key == "allowedLoginMethods" && value == &json!(["chatgpt"]))
            })
        });
    config["apps"].is_null()
        && config["approval_policy"].is_null()
        && config["approvals_reviewer"].is_null()
        && login_only
}
async fn inventory(rpc: &mut Rpc, thread: &str) -> Result<Value> {
    let account = rpc
        .request_guarded("account/read", json!({"refreshToken":false}))
        .await?;
    let account_key = fingerprint(&account["account"])?;
    let installed = rpc
        .request_guarded(
            "app/installed",
            json!({"forceRefresh":true,"threadId":thread}),
        )
        .await?;
    // Do not query app/list: it also fetches the public store directory, whose
    // browser challenge can fail independently of the installed connector runtime.
    let installed_rows = installed["apps"]
        .as_array()
        .context("Invalid Codex installed-app snapshot")?;
    ensure!(
        installed_rows.len() <= 200,
        "Codex installed-app limit exceeded"
    );
    let mut ids = HashSet::new();
    for row in installed_rows {
        let id = runtime::string(row, "id")?;
        ensure!(
            !id.is_empty() && id.len() <= 256 && ids.insert(id),
            "Invalid or duplicate Codex app identity"
        );
    }
    if installed_rows.is_empty() {
        return Ok(json!({"account_key":account_key,"connections":[]}));
    }
    let servers = page(
        rpc,
        "mcpServerStatus/list",
        json!({"detail":"full","limit":100,"threadId":thread}),
    )
    .await?;
    let ids: Vec<_> = installed_rows.iter().map(|a| a["id"].clone()).collect();
    let mut metadata = Vec::new();
    let mut warning = None;
    // app/read accepts at most 100 ids. Display metadata must never substitute
    // for live installed state or fabricate a callable binding on failure.
    for ids in ids.chunks(100) {
        match rpc
            .request_guarded(
                "app/read",
                json!({"appIds":ids,"includeTools":true,"threadId":thread}),
            )
            .await
        {
            Ok(value) => metadata.extend(
                value["apps"]
                    .as_array()
                    .context("Invalid Codex app metadata")?
                    .iter()
                    .cloned(),
            ),
            Err(error) => {
                metadata.clear();
                warning = Some(format!(
                    "App display metadata could not be loaded. {} Tools with verified runtime app identifiers remain available; unverified tools are unavailable.",
                    error
                ));
                break;
            }
        }
    }
    let config = rpc
        .request_guarded(
            "config/read",
            json!({"includeLayers":false,"cwd":"/workspace"}),
        )
        .await
        .ok();
    let requirements = rpc
        .request_guarded("configRequirements/read", json!({}))
        .await
        .ok();
    let default_policy = provider_default_policy(config.as_ref(), requirements.as_ref());
    let mut rows = connections(&json!({"apps":metadata}), &installed, &servers)?;
    for row in &mut rows {
        row["provider_requires_review"] = json!(!default_policy);
        if !default_policy {
            row["permission_persistence_available"] = json!(false);
            row["availability_message"] = json!(
                "This Codex connection has an explicit or unverified provider policy. Each call needs review."
            );
        }
    }
    Ok(json!({"account_key":account_key,"connections":rows,"warning":warning}))
}
pub async fn refresh(app: &App) -> Result<Value> {
    // A failed refresh must not leave a stale inventory advertised as callable.
    crate::connector_policy::clear_catalogue_for(&app.db, "codex-account")?;
    let mut rpc = Rpc::connect(&app.config.vm).await?;
    let thread = auxiliary(&mut rpc).await?;
    let value = inventory(&mut rpc, &thread).await?;
    crate::connector_policy::catalogue_for(
        &app.db,
        "codex-account",
        runtime::string(&value, "account_key")?,
        &value["connections"],
    )?;
    Ok(value)
}
/// Model-facing discovery is an app directory, not every app's full schema.
/// Keep the settings inventory unchanged and retain every permission/source gate.
pub fn discovery_view(mut value: Value) -> Value {
    if value["provider_source"] != "codex" {
        return value;
    }
    value["kindred_configured"] = value["configured"].clone();
    value.as_object_mut().unwrap().remove("configured");
    value["chatgpt_connected"] = json!(
        !value["provider_account_key"]
            .as_str()
            .unwrap_or("")
            .is_empty()
    );
    if let Some(rows) = value["provider_connections"].as_array_mut() {
        for row in rows {
            let count = row["tool_schemas"].as_array().map(Vec::len).unwrap_or(0);
            row.as_object_mut().unwrap().remove("tool_schemas");
            row.as_object_mut().unwrap().remove("tools");
            row["tool_count"] = json!(count);
            row["discover_tools"] =
                json!({"tool":"codex_connector_tools","arguments":{"app_id":row["app_id"]}});
        }
    }
    value["discovery_instruction"] = json!(
        "provider_connections lists ChatGPT apps; apps lists separate Kindred connections. An empty apps list or kindred_configured=false does not mean ChatGPT apps are missing. Use codex_connector_tools for a ChatGPT app, then codex_connector with its returned exact schema and identifiers. All Codex models, including Luna, use this route."
    );
    value
}
pub async fn tools(app: &App, bot: &Bot, args: &Value) -> Result<Value> {
    ensure!(
        bot.provider == "codex" && app.db.bot(&bot.id)?.provider == "codex",
        "ChatGPT app discovery requires a Codex subscription bot"
    );
    refresh(app).await?;
    tool_page(&crate::connector_policy::inventory(app, bot)?, args)
}
fn tool_page(inv: &Value, args: &Value) -> Result<Value> {
    const PAGE_BYTES: usize = 48_000;
    const LARGE_SCHEMA_BYTES: usize = 256_000;
    let app_id = runtime::string(args, "app_id")?;
    let query = args["query"].as_str().unwrap_or("").trim().to_lowercase();
    let cursor = args["cursor"].as_str().unwrap_or("");
    ensure!(
        app_id.len() <= 256 && query.len() <= 300 && cursor.len() <= 128,
        "Invalid ChatGPT tool search"
    );
    let row = inv["provider_connections"]
        .as_array()
        .context("Refresh ChatGPT connections first")?
        .iter()
        .find(|v| v["app_id"] == app_id)
        .context("Choose an app_id from connectors_list → provider_connections")?;
    ensure!(
        row["execution_available"] == true,
        "This ChatGPT app has no verified callable tools. Check its connection in Settings."
    );
    let terms = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 1)
        .collect::<Vec<_>>();
    let mut all = row["tool_schemas"]
        .as_array()
        .context("ChatGPT app schemas unavailable")?
        .iter()
        .map(|v| {
            let name = v["name"].as_str().unwrap_or("").to_lowercase();
            let haystack = format!(
                "{} {}",
                v["name"].as_str().unwrap_or(""),
                v["description"].as_str().unwrap_or("")
            )
            .to_lowercase();
            // A tool mentioned in another action's prose is a weaker match
            // than the action's own name (for example read_thread vs edit_message).
            let score = terms.iter().map(|term| {
                if name.contains(*term) { 3 } else if haystack.contains(*term) { 1 } else { 0 }
            }).sum::<usize>();
            (score, v)
        })
        .filter(|(score, _)| terms.is_empty() || *score > 0)
        .collect::<Vec<_>>();
    all.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["name"].as_str().cmp(&b.1["name"].as_str()))
    });
    let binding = json!([
        inv["provider_account_key"],
        app_id,
        query,
        all.iter().map(|(_, v)| v).collect::<Vec<_>>()
    ]);
    let fingerprint = digest(&SHA256, binding.to_string().as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let offset = if cursor.is_empty() {
        0
    } else {
        let (prefix, index) = cursor
            .split_once(':')
            .context("Invalid ChatGPT tool cursor")?;
        ensure!(
            prefix == fingerprint,
            "ChatGPT tools or account changed. Search again without the old cursor."
        );
        index
            .parse::<usize>()
            .context("Invalid ChatGPT tool cursor")?
    };
    ensure!(offset <= all.len(), "Invalid ChatGPT tool cursor");
    let mut selected = Vec::new();
    let mut unavailable = Vec::new();
    let mut bytes = 0;
    let mut next = offset;
    for (_, value) in all.iter().skip(offset).take(4) {
        let size = value.to_string().len();
        // Never let one large schema poison every subsequent page. Return a
        // complete supported schema alone; never truncate its required fields,
        // enums or constraints. Execution still validates against the live copy.
        if size > LARGE_SCHEMA_BYTES {
            unavailable.push(json!({"name":value["name"],"schema_bytes":size,
                "reason":"This schema is too large to expose in one discovery response. Its arguments were not returned; do not guess them. Other actions remain available."}));
            next += 1;
            continue;
        }
        if !selected.is_empty() && bytes + size > PAGE_BYTES {
            break;
        }
        bytes += size;
        selected.push(*value);
        next += 1;
        if bytes > PAGE_BYTES { break; }
    }
    Ok(
        json!({"app_id":app_id,"server":row["server"],"account_key":inv["provider_account_key"],"connection":row["display_name"],"requires_approval":row["requires_approval"],"permission_persistence_available":row["permission_persistence_available"],"execution_available":true,"tools":selected,"unavailable_tools":unavailable,"matched_tools":all.len(),"next_cursor":if next<all.len(){Some(format!("{fingerprint}:{next}"))}else{None},"execute_with":"codex_connector","note":"Use exact returned names and complete inputSchema from tools. unavailable_tools contain no usable schema. Follow next_cursor to continue; discovery does not grant action permission."}),
    )
}
fn binding(inv: &Value, args: &Value) -> Result<Value> {
    ensure!(
        inv["account_key"] == args["account_key"],
        "Codex account changed. Refresh its connectors and review again."
    );
    let app_id = runtime::string(args, "app_id")?;
    let server = runtime::string(args, "server")?;
    let tool = runtime::string(args, "tool_name")?;
    let row = inv["connections"]
        .as_array()
        .context("Missing Codex inventory")?
        .iter()
        .find(|r| {
            r["app_id"] == app_id && r["server"] == server && r["execution_available"] == true
        })
        .context("This Codex app is unavailable or its provenance is unresolved")?;
    let schema = row["tool_schemas"]
        .as_array()
        .context("Missing Codex tool schemas")?
        .iter()
        .find(|s| s["name"] == tool)
        .context("Unknown or unavailable Codex app tool")?;
    Ok(
        json!({"app_id":app_id,"server":server,"connection":row["display_name"],"connector_key":row["connector_key"],"permission_persistence_available":row["permission_persistence_available"],"provider_requires_review":row["provider_requires_review"],"tool":schema}),
    )
}
fn current_bot(app: &App, bot: &Bot, run: &Run) -> Result<()> {
    ensure!(
        run.bot_id == bot.id && bot.provider == "codex" && app.db.bot(&bot.id)?.provider == "codex",
        "This task no longer uses Codex connectors"
    );
    ensure!(!app.db.cancelled(&run.id), "Task cancelled");
    Ok(())
}
pub async fn execute(app: &App, bot: &Bot, run: &Run, args: &Value) -> Result<Value> {
    current_bot(app, bot, run)?;
    let mut rpc = Rpc::connect(&app.config.vm).await?;
    execute_rpc(app, bot, run, args, &mut rpc).await
}
async fn execute_rpc(
    app: &App,
    bot: &Bot,
    run: &Run,
    args: &Value,
    rpc: &mut Rpc,
) -> Result<Value> {
    current_bot(app, bot, run)?;
    ensure!(
        args["arguments"].is_object() && args["arguments"].to_string().len() <= 200_000,
        "Invalid Codex connector arguments"
    );
    let thread = auxiliary(rpc).await?;
    let inv = inventory(rpc, &thread).await?;
    let identity = binding(&inv, args)?;
    crate::composio::validate_arguments(&identity["tool"]["inputSchema"], &args["arguments"])?;
    crate::connector_policy::catalogue_for(
        &app.db,
        "codex-account",
        runtime::string(&inv, "account_key")?,
        &inv["connections"],
    )?;
    let force = identity["permission_persistence_available"] != true;
    let mut review = json!({"origin":"codex-account","account_key":args["account_key"],"connector_key":identity["connector_key"],"connection":identity["connection"],"account_name":"Linked account selected by Codex (not independently identified)","tool_name":args["tool_name"],"arguments":args["arguments"],"forced":force,"permission_persistence_available":!force,"read_only":identity["tool"]["read_only"],"app_id":args["app_id"],"server":args["server"]});
    let id = connector_artifacts::create(&app.db, run, &review)?;
    review["artifact_id"] = json!(id);
    if !connector_artifacts::review(app, bot, run, "codex_connector", &mut review, force).await? {
        return Ok(
            json!({"text":"The user declined this action. Do not repeat it or switch sources.","failed":true}),
        );
    }
    let result:Result<Value>=async {
        current_bot(app,bot,run)?;
        let fresh=inventory(rpc,&thread).await?;
        ensure!(binding(&fresh,args)?==identity,"Codex tool changed after review. Nothing was dispatched; refresh and review again.");
        crate::composio::validate_arguments(&identity["tool"]["inputSchema"],&review["arguments"])?;
        current_bot(app,bot,run)?;
        connector_artifacts::dispatch(&app.db,run,&review)?;
        app.db.event(&run.id,"tool_started",json!({"tool":"codex_connector","args":review}))?;
        let value=rpc.request_guarded("mcpServer/tool/call",json!({"server":args["server"],"threadId":thread,"tool":args["tool_name"],"arguments":review["arguments"]})).await?;
        ensure!(value["content"].is_array(),"Invalid connector result; the outcome is uncertain. Do not retry a write.");
        Ok(value)
    }.await;
    match result {
        Ok(value) => {
            let failed = value["isError"] == true;
            connector_artifacts::complete(&app.db, &id, &value, failed)?;
            Ok(json!({"text":runtime::bounded(&value.to_string(),60_000),"failed":failed}))
        }
        Err(e) => {
            let value = json!({"text":e.to_string(),"failed":true});
            connector_artifacts::complete(&app.db, &id, &value, true)?;
            Ok(value)
        }
    }
}
