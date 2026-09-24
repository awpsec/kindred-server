//! Imported Codex apps execute only through reviewed, turnless auxiliary RPCs.
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
        if available && metas_for.len() == 1 {
            for summary in metas_for[0]["toolSummaries"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let Some(tool) = summary["name"].as_str() else {
                    continue;
                };
                if summary["isEnabled"] == false
                    || !owners
                        .get(tool)
                        .is_some_and(|o| o.len() == 1 && o.contains(id))
                {
                    continue;
                }
                let Some(schema) = server.and_then(|s| s["tools"].get(tool)) else {
                    continue;
                };
                if schema["name"] != tool || !schema["inputSchema"].is_object() {
                    continue;
                }
                tools.push(json!({"name":tool,"description":summary["description"],"inputSchema":schema["inputSchema"],"read_only":summary["isReadOnly"]==true && schema["annotations"]["readOnlyHint"]==true}));
            }
        }
        let executable = !tools.is_empty();
        rows.push(json!({"origin":"codex-account","name":format!("codex {name}"),"display_name":name,"connector_key":id,"app_id":id,"server":"codex_apps","status":if executable{"connected"}else{"unavailable"},"execution_available":executable,"permission_persistence_available":false,"requires_approval":true,"always_allow":false,"linked_account_verified":false,"tools":tools.iter().map(|t|t["name"].clone()).collect::<Vec<_>>(),"tool_schemas":tools,"availability_message":if executable{"Each call needs review; Codex does not identify the linked service account."}else{"No unambiguous callable tool binding is available. Refresh or reconnect this app in Codex."}}));
    }
    Ok(rows)
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
                    "Installed apps were found, but their tools could not be verified. {} These connectors are unavailable until a refresh succeeds.",
                    error
                ));
                break;
            }
        }
    }
    let rows = connections(&json!({"apps":metadata}), &installed, &servers)?;
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
    Ok(json!({"app_id":app_id,"server":server,"connection":row["display_name"],"tool":schema}))
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
    let mut review = json!({"origin":"codex-account","account_key":args["account_key"],"connector_key":args["app_id"],"connection":identity["connection"],"account_name":"Linked account selected by Codex (not independently identified)","tool_name":args["tool_name"],"arguments":args["arguments"],"forced":true,"permission_persistence_available":false,"read_only":identity["tool"]["read_only"],"app_id":args["app_id"],"server":args["server"]});
    let id = connector_artifacts::create(&app.db, run, &review)?;
    review["artifact_id"] = json!(id);
    if !connector_artifacts::review(app, bot, run, "codex_connector", &mut review, true).await? {
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
