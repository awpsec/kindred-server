//! Bot-scoped connector routing and explicit, account-bound permissions.
use crate::{
    db::{Bot, Db, Run},
    runtime::{self, App, Shared},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS connector_preferences(bot_id TEXT PRIMARY KEY, source TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS connector_grants(bot_id TEXT NOT NULL, origin TEXT NOT NULL, account_key TEXT NOT NULL, connector_key TEXT NOT NULL, permission TEXT NOT NULL, PRIMARY KEY(bot_id,origin,account_key,connector_key));
        CREATE TABLE IF NOT EXISTS connector_catalogue(origin TEXT PRIMARY KEY, account_key TEXT NOT NULL, connections TEXT NOT NULL);")?;
    Ok(())
}
fn key(v: &Value, name: &str) -> Result<String> {
    let s = runtime::string(v, name)?;
    ensure!(
        !s.is_empty() && s.len() <= 256,
        "Invalid connector identity"
    );
    Ok(s.into())
}
pub fn catalogue(db: &Db, account: &str, rows: &Value) -> Result<()> {
    catalogue_for(db, "claude-account", account, rows)
}
pub fn provider_identity(provider: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match provider {
        "claude-code" => Some(("claude", "Claude", "claude-account")),
        "codex" => Some(("codex", "Codex", "codex-account")),
        _ => None,
    }
}
pub fn catalogue_for(db: &Db, origin: &str, account: &str, rows: &Value) -> Result<()> {
    ensure!(
        matches!(origin, "claude-account" | "codex-account"),
        "Invalid provider connector origin"
    );
    ensure!(
        account.len() == 64 && account.bytes().all(|b| b.is_ascii_hexdigit()),
        "Provider account identity is unavailable. Reconnect the provider."
    );
    let rows = rows.as_array().context("Invalid connector catalogue")?;
    ensure!(rows.len() <= 200, "Connector catalogue limit");
    let mut keys = std::collections::HashSet::new();
    for row in rows {
        ensure!(
            row["origin"] == origin
                && runtime::string(row, "name")?.starts_with(if origin == "claude-account" {
                    "claude.ai "
                } else {
                    "codex "
                }),
            "Invalid connector origin"
        );
        ensure!(
            keys.insert(key(row, "connector_key")?),
            "Duplicate connector identity"
        );
    }
    db.0.lock().unwrap().execute("INSERT INTO connector_catalogue VALUES(?,?,?) ON CONFLICT(origin) DO UPDATE SET account_key=excluded.account_key,connections=excluded.connections", params![origin,account,json!(rows).to_string()])?;
    Ok(())
}
pub fn clear_catalogue(db: &Db) -> Result<()> {
    clear_catalogue_for(db, "claude-account")
}
pub fn clear_catalogue_for(db: &Db, origin: &str) -> Result<()> {
    ensure!(
        matches!(origin, "claude-account" | "codex-account"),
        "Invalid provider connector origin"
    );
    db.0.lock()
        .unwrap()
        .execute("DELETE FROM connector_catalogue WHERE origin=?", [origin])?;
    Ok(())
}
fn source(c: &Connection, bot: &str) -> Result<String> {
    let value: String = c
        .query_row(
            "SELECT source FROM connector_preferences WHERE bot_id=?",
            [bot],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "unconfigured".into());
    Ok(if matches!(value.as_str(), "claude" | "codex") {
        "provider".into()
    } else {
        value
    })
}
fn permission(
    c: &Connection,
    bot: &str,
    origin: &str,
    account: &str,
    connector: &str,
) -> Result<bool> {
    Ok(permission_override(c, bot, origin, account, connector)? == Some(true))
}
fn permission_override(
    c: &Connection,
    bot: &str,
    origin: &str,
    account: &str,
    connector: &str,
) -> Result<Option<bool>> {
    Ok(c.query_row("SELECT permission FROM connector_grants WHERE bot_id=? AND origin=? AND account_key=? AND connector_key IN (?, '*') ORDER BY (connector_key=?) DESC LIMIT 1",params![bot,origin,account,connector,connector],|r|r.get::<_,String>(0)).optional()?.map(|v|v=="allow"))
}
fn email_policy(
    c: &Connection,
    bot: &str,
    origin: &str,
    account: &str,
    connector: &str,
) -> Result<String> {
    let value:Option<String>=c.query_row("SELECT permission FROM connector_action_grants WHERE bot_id=? AND origin=? AND account_key=? AND connector_key=? AND action='email_send'",params![bot,origin,account,connector],|r|r.get(0)).optional()?;
    Ok(value.unwrap_or_else(|| "inherit".into()))
}
pub fn approval_override(db: &Db, bot: &str, args: &Value) -> Result<Option<bool>> {
    approval_override_locked(&db.0.lock().unwrap(), bot, args)
}
pub(crate) fn approval_override_locked(
    c: &Connection,
    bot: &str,
    args: &Value,
) -> Result<Option<bool>> {
    if args["forced"] == true || args["origin"] == "codex-account" {
        return Ok(Some(false));
    }
    let (Some(origin), Some(account), Some(connector)) = (
        args["origin"].as_str(),
        args["account_key"].as_str(),
        args["connector_key"].as_str(),
    ) else {
        return Ok(None);
    };
    if crate::connector_artifacts::email_send(args) {
        let scoped: Option<String> = c.query_row("SELECT permission FROM connector_action_grants WHERE bot_id=? AND origin=? AND account_key=? AND connector_key=? AND action='email_send'",params![bot,origin,account,connector],|r|r.get(0)).optional()?;
        if let Some(value) = scoped {
            return Ok(Some(value == "allow"));
        }
    }
    // A visible off switch means ask, including before any preference was saved.
    // Do not let the general Full access policy turn an absent connector grant on.
    Ok(Some(
        permission_override(c, bot, origin, account, connector)?.unwrap_or(false),
    ))
}
fn service(name: &str) -> String {
    let n: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match n.as_str() {
        "googlemail" => "gmail",
        "drive" => "googledrive",
        "calendar" => "googlecalendar",
        "mondaycom" => "monday",
        _ => &n,
    }
    .into()
}
pub fn duplicates(claude: &[Value], kindred: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for app in kindred["apps"].as_array().into_iter().flatten() {
        let id = service(app["id"].as_str().unwrap_or(""));
        if id.is_empty()
            || !app["accounts"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|a| a["status"] == "ACTIVE")
        {
            continue;
        }
        for row in claude.iter().filter(|r| r["status"] == "connected") {
            let exact = service(row["display_name"].as_str().unwrap_or("")) == id;
            let atlassian = row["display_name"].as_str() == Some("Atlassian")
                && matches!(id.as_str(), "confluence" | "jira")
                && row["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|t| t.as_str().unwrap_or("").to_lowercase().contains(&id));
            if exact || atlassian {
                out.push(json!({"service":id,"name":app["name"],"provider_connection":row["display_name"],"provider_origin":row["origin"],"claude_connection":row["display_name"],"connector_key":row["connector_key"],"kindred_accounts":app["accounts"]}));
                break;
            }
        }
    }
    out
}
pub fn inventory(app: &App, bot: &Bot) -> Result<Value> {
    let mut result = crate::composio::status(app)?;
    let c = app.db.0.lock().unwrap();
    result["preferred_source"] = json!(source(&c, &bot.id)?);
    let identity = provider_identity(&bot.provider);
    let origin = identity.map(|v| v.2).unwrap_or("");
    let cached: Option<(String, String)> = if identity.is_some() {
        c.query_row(
            "SELECT account_key,connections FROM connector_catalogue WHERE origin=?",
            [origin],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    } else {
        None
    };
    let (account, rows) = cached
        .map(|(a, r)| {
            (
                a,
                serde_json::from_str::<Vec<Value>>(&r).unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    result["duplicates"] = json!(duplicates(&rows, &result));
    result["provider_source"] = json!(identity.map(|v| v.0));
    result["provider_name"] = json!(identity.map(|v| v.1));
    result["provider_origin"] = json!(identity.map(|v| v.2));
    result["provider_account_key"] = json!(account);
    result["provider_all_allowed"] =
        json!(origin == "claude-account" && permission(&c, &bot.id, origin, &account, "*")?);
    result["provider_permission_persistence_available"] = json!(origin == "claude-account");
    result["resolved_preferred_source"] = if result["preferred_source"] == "provider" {
        json!(identity.map(|v| v.0))
    } else {
        result["preferred_source"].clone()
    };
    let mut rows = rows;
    for row in &mut rows {
        row["always_allow"] = json!(
            row["permission_persistence_available"] != false
                && permission(
                    &c,
                    &bot.id,
                    origin,
                    &account,
                    row["connector_key"].as_str().unwrap_or("")
                )?
        );
        row["execution_permission"] = json!(if row["always_allow"] == true {
            "always_allow"
        } else {
            "ask"
        });
        row["requires_approval"] = json!(row["always_allow"] != true);
        if matches!(
            service(row["display_name"].as_str().unwrap_or("")).as_str(),
            "gmail" | "outlook" | "microsoftoutlook"
        ) {
            row["email_sending"] = json!(email_policy(
                &c,
                &bot.id,
                origin,
                &account,
                row["connector_key"].as_str().unwrap_or("")
            )?);
        }
    }
    result["provider_connections"] = json!(rows);
    // Keep older clients working while all new UI and routing use the provider fields.
    result["claude_account_key"] = if origin == "claude-account" {
        json!(account)
    } else {
        json!("")
    };
    result["claude_all_allowed"] =
        json!(origin == "claude-account" && result["provider_all_allowed"] == true);
    result["claude_connections"] = if origin == "claude-account" {
        json!(rows)
    } else {
        json!([])
    };
    if let Some(apps) = result["apps"].as_array_mut() {
        for app in apps {
            let toolkit = app["id"].as_str().unwrap_or("").to_string();
            if let Some(accounts) = app["accounts"].as_array_mut() {
                for a in accounts {
                    a["always_allow"] = json!(permission(
                        &c,
                        &bot.id,
                        "kindred",
                        a["id"].as_str().unwrap_or(""),
                        &toolkit
                    )?);
                    a["execution_permission"] = json!(if a["always_allow"] == true {
                        "always_allow"
                    } else {
                        "ask"
                    });
                    a["requires_approval"] = json!(a["always_allow"] != true);
                    if matches!(toolkit.as_str(), "gmail" | "outlook" | "microsoftoutlook") {
                        a["email_sending"] = json!(email_policy(
                            &c,
                            &bot.id,
                            "kindred",
                            a["id"].as_str().unwrap_or(""),
                            &toolkit
                        )?);
                    }
                }
            }
        }
    }
    result["routing_instruction"] = json!(
        "Connected describes account authentication, not permission to act. Prefer provider means the current bot's provider_source: Codex connections for a Codex bot, Claude connections for a Claude bot. Kindred connections remain available alongside provider connections or alone. For duplicate services use preferred_source unless the user explicitly selects another source; ask when unconfigured or ask. A missing or unavailable preferred provider connection requires an explanation and a choice, not silent fallback. Source preference grants no permissions. When requires_approval is true or always_allow is false, actions require approval even in Full access mode. Read-only account limits, email-specific policies, and provider-forced reviews still apply. execution_available=false means the connector cannot execute. permission_persistence_available=false means each call needs review and no standing grant can be saved. Never switch sources after denial or authorization failure. Use connector_configure to propose saved preferences or permissions; only the person's approval saves them."
    );
    Ok(result)
}
fn grant(c: &Connection, bot: &str, args: &Value, allow: bool) -> Result<()> {
    let origin = key(args, "origin")?;
    let account = key(args, "account_key")?;
    let connector = key(args, "connector_key")?;
    ensure!(
        matches!(
            origin.as_str(),
            "claude-account" | "codex-account" | "kindred"
        ),
        "Invalid connector source"
    );
    ensure!(
        args["forced"] != true,
        "This provider requirement must be approved each time"
    );
    if origin != "kindred" {
        let provider: String =
            c.query_row("SELECT provider FROM bots WHERE id=?", [bot], |r| r.get(0))?;
        ensure!(
            provider_identity(&provider).is_some_and(|v| v.2 == origin),
            "This bot no longer uses that provider"
        );
        let (current, rows): (String, String) = c.query_row(
            "SELECT account_key,connections FROM connector_catalogue WHERE origin=?",
            [&origin],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let rows: Vec<Value> = serde_json::from_str(&rows)?;
        ensure!(
            current == account
                && (connector == "*" || rows.iter().any(|r| r["connector_key"] == connector)),
            "Provider connection changed. Refresh its connectors."
        );
        ensure!(
            !allow || origin != "codex-account",
            "Codex connections require per-call review until the runtime supplies stable linked-account identity; source preference does not grant permission"
        );
    } else {
        ensure!(connector != "*", "Choose a specific Kindred connection");
        let stored: String =
            c.query_row("SELECT value FROM settings WHERE key='composio'", [], |r| {
                r.get(0)
            })?;
        let settings: crate::composio::Settings = serde_json::from_str(&stored)?;
        ensure!(
            settings
                .accounts
                .values()
                .any(|a| a.id == account && a.toolkit == connector),
            "Kindred connection changed. Refresh its connectors."
        );
    }
    if connector == "*" {
        c.execute(
            "DELETE FROM connector_grants WHERE bot_id=? AND origin=? AND account_key=?",
            params![bot, origin, account],
        )?;
    }
    c.execute("INSERT INTO connector_grants VALUES(?,?,?,?,?) ON CONFLICT(bot_id,origin,account_key,connector_key) DO UPDATE SET permission=excluded.permission",params![bot,origin,account,connector,if allow{"allow"}else{"ask"}])?;
    Ok(())
}
fn preference(c: &Connection, bot: &str, value: &str) -> Result<()> {
    let value = if matches!(value, "claude" | "codex") {
        "provider"
    } else {
        value
    };
    ensure!(
        matches!(value, "provider" | "kindred" | "ask"),
        "Choose provider, Kindred or ask"
    );
    c.execute("INSERT INTO connector_preferences VALUES(?,?) ON CONFLICT(bot_id) DO UPDATE SET source=excluded.source",params![bot,value])?;
    Ok(())
}
pub fn decide(db: &Db, id: &str, approved: bool, choice: &str) -> Result<()> {
    decide_checked(db, id, approved, choice, None, None)
}
pub fn decide_checked(
    db: &Db,
    id: &str,
    approved: bool,
    choice: &str,
    revision: Option<i64>,
    feedback: Option<&str>,
) -> Result<()> {
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let (bot,tool,args):(String,String,String)=tx.query_row("SELECT r.bot_id,a.tool,a.args FROM approvals a JOIN runs r ON r.id=a.run_id WHERE a.id=? AND a.status='pending' AND r.status='awaiting_approval'",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).context("Approval is no longer pending")?;
    let args: Value = serde_json::from_str(&args)?;
    let artifact = args["artifact_id"].as_str();
    if let Some(artifact) = artifact {
        let card = crate::connector_artifacts::record(&tx, artifact)?;
        ensure!(
            revision == card["revision"].as_i64()
                && card["status"] == "pending"
                && card["approval_id"] == id,
            "This draft changed. Review the current card before approving."
        );
    } else {
        ensure!(
            revision.is_none() && feedback.is_none(),
            "This action is not a connector card"
        );
    }
    if let Some(feedback) = feedback {
        ensure!(
            !approved && choice.is_empty() && !feedback.trim().is_empty() && feedback.len() <= 8000,
            "Describe the changes before returning this draft"
        );
    }
    if approved {
        if tool == "connector_configure" {
            if args["request"] == "source_preference" {
                preference(
                    &tx,
                    &bot,
                    if choice.is_empty() {
                        args["source"].as_str().unwrap_or("")
                    } else {
                        choice
                    },
                )?;
            } else if args["request"] == "email_sending" {
                save_email_permission(&tx, &bot, &args, args["always_allow"] == true)?;
            } else {
                ensure!(
                    args["request"] == "always_allow",
                    "Invalid connector proposal"
                );
                grant(&tx, &bot, &args, true)?;
            }
        } else if choice == "always_allow_email" {
            ensure!(
                matches!(
                    tool.as_str(),
                    "claude_connector" | "codex_connector" | "connector_execute"
                ) && crate::connector_artifacts::email_send(&args),
                "This is not an email sending approval"
            );
            save_email_permission(&tx, &bot, &args, true)?;
        } else if choice == "always_allow" {
            ensure!(
                matches!(
                    tool.as_str(),
                    "claude_connector" | "codex_connector" | "connector_execute"
                ),
                "Not a connector approval"
            );
            grant(&tx, &bot, &args, true)?;
        } else {
            ensure!(choice.is_empty(), "Invalid approval choice");
        }
    } else {
        ensure!(choice.is_empty(), "A denial cannot save permission");
    }
    tx.execute(
        "UPDATE approvals SET status=? WHERE id=?",
        params![if approved { "approved" } else { "denied" }, id],
    )?;
    if let Some(id) = artifact {
        let mut card = crate::connector_artifacts::record(&tx, id)?;
        if let Some(feedback) = feedback {
            card["feedback"] = json!(feedback.trim());
            tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'user',?,'message',?,?)",params![card["chat_id"].as_str(),format!("Changes requested for {}: {}",card["title"].as_str().unwrap_or("connector action"),feedback.trim()),card["run_id"].as_str(),crate::db::now()])?;
        }
        tx.execute(
            "UPDATE connector_artifacts SET body=?,status=?,revision=revision+1 WHERE id=?",
            params![
                card.to_string(),
                if feedback.is_some() {
                    "changes_requested"
                } else if approved {
                    "approved"
                } else {
                    "denied"
                },
                id
            ],
        )?;
    }
    tx.execute(
        "UPDATE runs SET status='running' WHERE id=(SELECT run_id FROM approvals WHERE id=?)",
        [id],
    )?;
    tx.commit()?;
    Ok(())
}
fn save_email_permission(c: &Connection, bot: &str, args: &Value, allow: bool) -> Result<()> {
    ensure!(
        matches!(
            crate::connector_artifacts::service(args).as_str(),
            "gmail" | "outlook" | "microsoftoutlook"
        ),
        "Choose a supported email connection"
    );
    // Reuse live identity validation without expanding connector-wide access.
    let origin = key(args, "origin")?;
    let account = key(args, "account_key")?;
    let connector = key(args, "connector_key")?;
    ensure!(
        !allow || origin != "codex-account",
        "Codex connections require per-call review"
    );
    ensure!(
        connector != "*" && args["forced"] != true,
        "Choose one email connection; required approvals cannot be bypassed"
    );
    c.execute_batch("SAVEPOINT validate_email_identity")?;
    let validated = grant(c, bot, args, false);
    c.execute_batch("ROLLBACK TO validate_email_identity; RELEASE validate_email_identity")?;
    validated?;
    c.execute("INSERT INTO connector_action_grants VALUES(?,?,?,?,'email_send',?) ON CONFLICT(bot_id,origin,account_key,connector_key,action) DO UPDATE SET permission=excluded.permission",params![bot,origin,account,connector,if allow{"allow"}else{"ask"}])?;
    Ok(())
}
pub async fn configure(app: &App, bot: &Bot, run: &Run, args: &Value) -> Result<Value> {
    let inv = inventory(app, bot)?;
    let proposal = if args["request"] == "email_sending" {
        let source = runtime::string(args, "source")?;
        let connector = runtime::string(args, "connector_key")?;
        let allow = match runtime::string(args, "policy")? {
            "allow" => true,
            "ask" => false,
            _ => anyhow::bail!("Choose allow or ask"),
        };
        let mut proposal = if matches!(source, "provider" | "claude" | "codex") {
            ensure!(
                source == "provider" || inv["provider_source"] == source,
                "This bot uses a different provider"
            );
            let row = inv["provider_connections"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| r["connector_key"] == connector)
                .context("Choose a current provider email connection")?;
            ensure!(
                !allow || row["permission_persistence_available"] != false,
                "This connection requires per-call review"
            );
            json!({"request":"email_sending","origin":inv["provider_origin"],"account_key":inv["provider_account_key"],"connector_key":connector,"connection":row["display_name"]})
        } else {
            ensure!(source == "kindred", "Choose provider or Kindred");
            let account = runtime::string(args, "account_key")?;
            let app = inv["apps"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| a["id"] == connector)
                .context("Choose a current Kindred email connection")?;
            let account = app["accounts"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| a["id"] == account)
                .context("Choose the connected email account")?;
            json!({"request":"email_sending","origin":"kindred","account_key":account["id"],"connector_key":connector,"toolkit":connector,"account_name":account["name"]})
        };
        proposal["always_allow"] = json!(allow);
        if !allow {
            save_email_permission(&app.db.0.lock().unwrap(), &bot.id, &proposal, false)?;
            return Ok(
                json!({"text":json!({"saved":true,"message":"This bot must ask before sending email through that connection. Other connector permissions are unchanged.","settings":inventory(app,bot)?}).to_string()}),
            );
        }
        proposal
    } else if args["request"] == "source_preference" {
        let source = runtime::string(args, "source")?;
        ensure!(
            matches!(source, "provider" | "claude" | "codex" | "kindred" | "ask"),
            "Invalid source preference"
        );
        json!({"request":"source_preference","source":if matches!(source,"claude"|"codex") {"provider"} else {source},"provider_name":inv["provider_name"],"duplicates":inv["duplicates"]})
    } else {
        ensure!(
            args["request"] == "always_allow"
                && (args["source"] == "provider" || args["source"] == "claude")
                && bot.provider == "claude-code",
            "Standing provider permissions are currently available only for Claude account connections; Codex requires per-call review"
        );
        let connector = if args["scope"] == "source" {
            "*"
        } else {
            runtime::string(args, "connector_key")?
        };
        let row = inv["claude_connections"]
            .as_array()
            .context("Refresh Claude connectors first")?
            .iter()
            .find(|r| r["connector_key"] == connector);
        ensure!(
            connector == "*" || row.is_some(),
            "Unknown Claude connector"
        );
        ensure!(
            inv["claude_account_key"].as_str().unwrap_or("").len() == 64,
            "Refresh Claude connectors first"
        );
        if (connector == "*" && inv["claude_all_allowed"] == true)
            || row.is_some_and(|r| r["always_allow"] == true)
        {
            return Ok(json!({"text":"This bot already has Always allow for that scope."}));
        }
        json!({"request":"always_allow","origin":"claude-account","account_key":inv["claude_account_key"],"connector_key":connector,"connection":row.map(|r|r["display_name"].clone()),"connections":inv["claude_connections"]})
    };
    let approved =
        runtime::approve_required(app, bot, run, "connector_configure", &proposal, true).await?;
    Ok(
        json!({"text":json!({"saved":approved,"settings":inventory(app,bot)?,"message":if approved{"The confirmed settings are saved for this bot."}else{"The user declined. Do not repeat the request or use another source."}}).to_string(),"failed":!approved}),
    )
}
pub async fn prepare(app: &App, bot: &Bot, run: &Run) -> Result<Value> {
    let mut inv = inventory(app, bot)?;
    if crate::provider_inbox::inventory_for_run(&app.db, run, &mut inv)? {
        return Ok(inv);
    }
    if inv["preferred_source"] == "unconfigured"
        && inv["duplicates"].as_array().is_some_and(|v| !v.is_empty())
    {
        let proposal = json!({"request":"source_preference","provider_name":inv["provider_name"],"duplicates":inv["duplicates"]});
        runtime::approve_required(app, bot, run, "connector_configure", &proposal, true).await?;
    }
    inventory(app, bot)
}
pub async fn get(
    State(app): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(inventory(&app, &app.db.bot(&id)?)?))
}
pub async fn put(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(save_settings(&app, &id, &v)?))
}
pub(crate) fn save_settings(app: &App, id: &str, v: &Value) -> Result<Value> {
    let bot = app.db.bot(&id)?;
    let inv = inventory(&app, &bot)?;
    let proposal =
        if v["request"] == "permission"
            || v["request"] == "email_sending"
            || v["request"] == "email_sending_reset"
        {
            let origin = key(&v, "origin")?;
            if origin == "kindred" {
                ensure!(
                    inv["apps"].as_array().into_iter().flatten().any(|a| a["id"]
                        == v["connector_key"]
                        && a["accounts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|a| a["id"] == v["account_key"])),
                    "Unknown Kindred connection"
                );
            }
            Some(v.clone())
        } else {
            None
        };
    {
        let mut c = app.db.0.lock().unwrap();
        let tx = c.transaction()?;
        if let Some(p) = proposal {
            if v["request"] == "email_sending_reset" {
                tx.execute_batch("SAVEPOINT reset_email_identity")?;
                let valid = grant(&tx, &id, &p, false);
                tx.execute_batch("ROLLBACK TO reset_email_identity; RELEASE reset_email_identity")?;
                valid?;
                tx.execute("DELETE FROM connector_action_grants WHERE bot_id=? AND origin=? AND account_key=? AND connector_key=? AND action='email_send'",params![id,key(&p,"origin")?,key(&p,"account_key")?,key(&p,"connector_key")?])?;
            } else if v["request"] == "email_sending" {
                save_email_permission(&tx, &id, &p, v["always_allow"] == true)?;
            } else {
                grant(&tx, &id, &p, v["always_allow"] == true)?;
            }
        } else {
            preference(&tx, &id, runtime::string(&v, "source")?)?;
        }
        tx.commit()?;
    }
    inventory(app, &bot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_preference_tracks_bot_provider_without_transferring_permissions() {
        let (app, mut bot, _) = fixture();
        shared(&app);
        catalogue_for(&app.db,"codex-account",&"b".repeat(64),&json!([{"origin":"codex-account","name":"codex Gmail","display_name":"Gmail","connector_key":"codex-gmail","status":"connected","permission_persistence_available":false,"execution_available":true}])).unwrap();
        // Legacy preferences retain their intent when the bot changes provider.
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO connector_preferences VALUES(?,'claude')",
                [&bot.id],
            )
            .unwrap();
        let inv = inventory(&app, &bot).unwrap();
        assert_eq!(inv["preferred_source"], "provider");
        assert_eq!(inv["resolved_preferred_source"], "claude");
        bot.provider = "codex".into();
        app.db.save_bot(&bot).unwrap();
        let inv = inventory(&app, &bot).unwrap();
        assert_eq!(inv["resolved_preferred_source"], "codex");
        assert_eq!(
            inv["provider_connections"][0]["connector_key"],
            "codex-gmail"
        );
        assert_eq!(inv["provider_all_allowed"], false);
        assert_eq!(inv["apps"].as_array().unwrap().len(), 2);
        let mut grant = json!({"request":"permission","origin":"codex-account","account_key":"b".repeat(64),"connector_key":"codex-gmail","connection":"Gmail","always_allow":true});
        assert!(save_settings(&app, &bot.id, &grant).is_err());
        grant["request"] = json!("email_sending");
        assert!(save_settings(&app, &bot.id, &grant).is_err());
        assert_eq!(
            approval_override(&app.db, &bot.id, &grant).unwrap(),
            Some(false)
        );
        bot.provider = "openrouter".into();
        app.db.save_bot(&bot).unwrap();
        let inv = inventory(&app, &bot).unwrap();
        assert!(inv["provider_connections"].as_array().unwrap().is_empty());
        assert!(inv["resolved_preferred_source"].is_null());
        assert_eq!(inv["apps"].as_array().unwrap().len(), 2);
    }
    fn rows() -> Value {
        json!([
            {"origin":"claude-account","name":"claude.ai Atlassian","display_name":"Atlassian","connector_key":"atlassian-1","status":"connected","tools":["mcp__claude_ai_Atlassian__searchConfluenceUsingCql","mcp__claude_ai_Atlassian__searchJira"]},
            {"origin":"claude-account","name":"claude.ai Gmail","display_name":"Gmail","connector_key":"gmail-1","status":"connected","tools":["mcp__claude_ai_Gmail__search"]}
        ])
    }
    fn args(connector: &str) -> Value {
        json!({"origin":"claude-account","account_key":"a".repeat(64),"connector_key":connector,"connection":"claude.ai Atlassian"})
    }
    fn fixture() -> (Shared, Bot, Run) {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "claude-code");
        catalogue(&app.db, &"a".repeat(64), &rows()).unwrap();
        let id = app.db.queue(&bot.id, "Test connectors", 0).unwrap();
        app.db.claim().unwrap();
        let run = app.db.run(&id).unwrap();
        (app, bot, run)
    }
    fn shared(app: &App) {
        app.db.save_setting("composio",&json!({"user_id":"fixture","configs":{},"accounts":{
            "gmail":{"id":"kindred-gmail-1","name":"Work","toolkit":"gmail","auth_config_id":"fixture","permission":"ask","status":"ACTIVE","checked_at":0,"scopes":[]},
            "confluence":{"id":"kindred-confluence-1","name":"Work wiki","toolkit":"confluence","auth_config_id":"fixture","permission":"ask","status":"ACTIVE","checked_at":0,"scopes":[]}
        }})).unwrap();
    }
    #[test]
    fn once_always_and_account_bot_isolation() {
        let (app, bot, run) = fixture();
        let a = args("atlassian-1");
        let id = app
            .db
            .request_approval(&run.id, "claude_connector", &a)
            .unwrap();
        decide(&app.db, &id, true, "").unwrap();
        assert_eq!(
            approval_override(&app.db, &bot.id, &a).unwrap(),
            Some(false)
        );
        let id = app
            .db
            .request_approval(&run.id, "claude_connector", &a)
            .unwrap();
        decide(&app.db, &id, true, "always_allow").unwrap();
        assert_eq!(approval_override(&app.db, &bot.id, &a).unwrap(), Some(true));
        assert!(decide(&app.db, &id, true, "always_allow").is_err());
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("gmail-1")).unwrap(),
            Some(false)
        );
        let other = crate::tests::bot(&app.db, "claude-code");
        assert_eq!(
            approval_override(&app.db, &other.id, &a).unwrap(),
            Some(false)
        );
        let mut changed = a.clone();
        changed["account_key"] = json!("b".repeat(64));
        assert_eq!(
            approval_override(&app.db, &bot.id, &changed).unwrap(),
            Some(false)
        );
        changed["account_key"] = a["account_key"].clone();
        changed["origin"] = json!("kindred");
        assert_eq!(
            approval_override(&app.db, &bot.id, &changed).unwrap(),
            Some(false)
        );
    }
    #[test]
    fn bulk_grant_individual_override_and_revoke_are_account_bound() {
        let (app, bot, run) = fixture();
        let mut all = args("*");
        all["request"] = json!("always_allow");
        let id = app
            .db
            .request_approval(&run.id, "connector_configure", &all)
            .unwrap();
        decide(&app.db, &id, true, "always_allow").unwrap();
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("gmail-1")).unwrap(),
            Some(true)
        );
        {
            let c = app.db.0.lock().unwrap();
            grant(&c, &bot.id, &args("gmail-1"), false).unwrap();
        }
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("gmail-1")).unwrap(),
            Some(false)
        );
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("atlassian-1")).unwrap(),
            Some(true)
        );
        {
            let c = app.db.0.lock().unwrap();
            grant(&c, &bot.id, &all, false).unwrap();
        }
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("atlassian-1")).unwrap(),
            Some(false)
        );
        {
            let c = app.db.0.lock().unwrap();
            grant(&c, &bot.id, &all, true).unwrap();
        }
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("gmail-1")).unwrap(),
            Some(true)
        );
        catalogue(&app.db, &"b".repeat(64), &rows()).unwrap();
        assert!(
            !inventory(&app, &bot).unwrap()["claude_all_allowed"]
                .as_bool()
                .unwrap()
        );
        let id = app
            .db
            .request_approval(&run.id, "connector_configure", &all)
            .unwrap();
        assert!(decide(&app.db, &id, true, "always_allow").is_err());
        assert_eq!(app.db.approval(&id).unwrap(), "pending");
        decide(&app.db, &id, false, "").unwrap();
    }
    #[test]
    fn rejected_cancelled_and_forced_requests_cannot_create_grants() {
        let (app, bot, run) = fixture();
        let a = args("atlassian-1");
        let id = app
            .db
            .request_approval(&run.id, "claude_connector", &a)
            .unwrap();
        assert!(decide(&app.db, &id, false, "always_allow").is_err());
        decide(&app.db, &id, false, "").unwrap();
        let mut forced = a.clone();
        forced["forced"] = json!(true);
        let id = app
            .db
            .request_approval(&run.id, "claude_connector", &forced)
            .unwrap();
        assert!(decide(&app.db, &id, true, "always_allow").is_err());
        decide(&app.db, &id, true, "").unwrap();
        let id = app.db.request_approval(&run.id, "guest_exec", &a).unwrap();
        assert!(decide(&app.db, &id, true, "always_allow").is_err());
        decide(&app.db, &id, false, "").unwrap();
        let id = app
            .db
            .request_approval(&run.id, "claude_connector", &a)
            .unwrap();
        app.db.cancel(&run.id).unwrap();
        assert!(decide(&app.db, &id, true, "always_allow").is_err());
        assert_eq!(
            approval_override(&app.db, &bot.id, &a).unwrap(),
            Some(false)
        );
    }
    #[test]
    fn duplicate_detection_matches_bundled_services_and_connected_accounts() {
        let (app, bot, _) = fixture();
        shared(&app);
        let inv = inventory(&app, &bot).unwrap();
        let d = inv["duplicates"].as_array().unwrap();
        assert_eq!(d.len(), 2);
        assert!(
            d.iter()
                .any(|d| d["service"] == "confluence" && d["claude_connection"] == "Atlassian")
        );
        let gpt = crate::tests::bot(&app.db, "codex");
        let inv = inventory(&app, &gpt).unwrap();
        assert!(inv["duplicates"].as_array().unwrap().is_empty());
        assert!(inv["claude_connections"].as_array().unwrap().is_empty());
        assert_eq!(inv["apps"].as_array().unwrap().len(), 2);
        let mut disconnected = rows();
        disconnected[0]["status"] = json!("needs-auth");
        assert_eq!(
            duplicates(
                disconnected.as_array().unwrap(),
                &crate::composio::status(&app).unwrap()
            )
            .len(),
            1
        );
    }
    #[tokio::test]
    async fn duplicate_choice_is_saved_before_context_and_does_not_grant_permission() {
        let (app, bot, run) = fixture();
        shared(&app);
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move { prepare(&a, &b, &r).await });
        let id = wait_approval(&app).await;
        assert_eq!(
            app.db.run_approvals(&run.id).unwrap()[0]["tool"],
            "connector_configure"
        );
        decide(&app.db, &id, true, "kindred").unwrap();
        let context = task.await.unwrap().unwrap();
        assert_eq!(context["preferred_source"], "kindred");
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("atlassian-1")).unwrap(),
            Some(false)
        );
        assert_eq!(
            prepare(&app, &bot, &run).await.unwrap()["preferred_source"],
            "kindred"
        );
        assert_eq!(app.db.run_approvals(&run.id).unwrap().len(), 1);
    }
    async fn wait_approval(app: &App) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(a) = app.db.approvals().unwrap().first() {
                    return a["id"].as_str().unwrap().into();
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn an_untouched_off_switch_requires_approval_even_with_full_access() {
        let (app, mut bot, run) = fixture();
        bot.approval_mode = "full".into();
        app.db.save_bot(&bot).unwrap();
        let inv = inventory(&app, &bot).unwrap();
        assert_eq!(inv["claude_connections"][0]["always_allow"], false);
        assert_eq!(inv["claude_connections"][0]["execution_permission"], "ask");
        assert_eq!(inv["claude_connections"][0]["requires_approval"], true);
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            runtime::approve_required(&a, &b, &r, "claude_connector", &args("gmail-1"), false).await
        });
        let id = wait_approval(&app).await;
        assert!(!task.is_finished());
        decide(&app.db, &id, false, "").unwrap();
        assert!(!task.await.unwrap().unwrap());
        bot.provider = "openrouter".into();
        bot.model = "fixture/model".into();
        app.db.save_bot(&bot).unwrap();
        let hidden = inventory(&app, &bot).unwrap();
        assert!(hidden["claude_connections"].as_array().unwrap().is_empty());
        assert_eq!(hidden["claude_account_key"], "");
    }
    #[tokio::test]
    async fn bulk_chat_confirmation_is_required_even_in_full_mode_then_covers_all_connections() {
        let (app, mut bot, run) = fixture();
        bot.approval_mode = "full".into();
        app.db.save_bot(&bot).unwrap();
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            configure(
                &a,
                &b,
                &r,
                &json!({"request":"always_allow","source":"claude","scope":"source"}),
            )
            .await
        });
        let id = wait_approval(&app).await;
        assert_eq!(
            approval_override(&app.db, &bot.id, &args("gmail-1")).unwrap(),
            Some(false)
        );
        decide(&app.db, &id, true, "always_allow").unwrap();
        let result = task.await.unwrap().unwrap();
        assert!(result["text"].as_str().unwrap().contains("\"saved\":true"));
        for key in ["gmail-1", "atlassian-1"] {
            assert!(
                runtime::approve_required(&app, &bot, &run, "claude_connector", &args(key), false)
                    .await
                    .unwrap()
            );
        }
        assert_eq!(app.db.run_approvals(&run.id).unwrap().len(), 1);
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            runtime::approve_required(&a, &b, &r, "claude_connector", &args("gmail-1"), true).await
        });
        let id = wait_approval(&app).await;
        decide(&app.db, &id, false, "").unwrap();
        assert!(!task.await.unwrap().unwrap());
        save_settings(&app,&bot.id,&json!({"request":"permission","origin":"claude-account","account_key":"a".repeat(64),"connector_key":"gmail-1","always_allow":false})).unwrap();
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            runtime::approve_required(&a, &b, &r, "claude_connector", &args("gmail-1"), false).await
        });
        let id = wait_approval(&app).await;
        decide(&app.db, &id, false, "").unwrap();
        assert!(!task.await.unwrap().unwrap());
    }
    #[tokio::test]
    async fn chat_email_allow_requires_confirmation_and_ask_immediately_overrides_full_connection_access()
     {
        let (app, bot, run) = fixture();
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            configure(&a,&b,&r,&json!({"request":"email_sending","source":"claude","connector_key":"gmail-1","policy":"allow"})).await
        });
        let id = wait_approval(&app).await;
        let mut send = args("gmail-1");
        send["connection"] = json!("claude.ai Gmail");
        send["tool_name"] = json!("mcp__claude_ai_Gmail__send_email");
        assert_eq!(
            approval_override(&app.db, &bot.id, &send).unwrap(),
            Some(false)
        );
        decide(&app.db, &id, true, "").unwrap();
        assert!(task.await.unwrap().is_ok());
        assert_eq!(
            approval_override(&app.db, &bot.id, &send).unwrap(),
            Some(true)
        );
        grant(&app.db.0.lock().unwrap(), &bot.id, &args("*"), true).unwrap();
        configure(&app,&bot,&run,&json!({"request":"email_sending","source":"claude","connector_key":"gmail-1","policy":"ask"})).await.unwrap();
        assert_eq!(
            approval_override(&app.db, &bot.id, &send).unwrap(),
            Some(false)
        );
        let mut read = send.clone();
        read["tool_name"] = json!("mcp__claude_ai_Gmail__search");
        assert_eq!(
            approval_override(&app.db, &bot.id, &read).unwrap(),
            Some(true)
        );
        save_settings(&app,&bot.id,&json!({"request":"email_sending_reset","origin":"claude-account","account_key":"a".repeat(64),"connector_key":"gmail-1"})).unwrap();
        assert_eq!(
            approval_override(&app.db, &bot.id, &send).unwrap(),
            Some(true)
        );
        assert_eq!(app.db.run_approvals(&run.id).unwrap().len(), 1);
    }
    #[test]
    fn kindred_grants_validate_actual_account_and_remain_bot_specific() {
        let (app, bot, _) = fixture();
        shared(&app);
        let v = json!({"request":"permission","origin":"kindred","account_key":"kindred-gmail-1","connector_key":"gmail","always_allow":true});
        save_settings(&app, &bot.id, &v).unwrap();
        assert_eq!(approval_override(&app.db, &bot.id, &v).unwrap(), Some(true));
        let mut wrong = v.clone();
        wrong["account_key"] = json!("missing");
        assert!(save_settings(&app, &bot.id, &wrong).is_err());
        let other = crate::tests::bot(&app.db, "codex");
        assert_eq!(
            approval_override(&app.db, &other.id, &v).unwrap(),
            Some(false)
        );
    }
}
