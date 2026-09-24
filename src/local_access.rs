//! Outbound, device-bound desktop work. A claimed operation is never requeued.
use crate::{
    db::{self, Bot, Run},
    runtime::{App, Shared},
};
use anyhow::{Context, Result, ensure};
use axum::{Json, extract::State};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS local_device_names(device_id TEXT PRIMARY KEY,name TEXT NOT NULL);")?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS local_devices(id TEXT PRIMARY KEY,secret TEXT NOT NULL,name TEXT NOT NULL,mode TEXT NOT NULL,seen INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS local_requests(id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES runs(id),bot_id TEXT NOT NULL REFERENCES bots(id),device_id TEXT NOT NULL REFERENCES local_devices(id),tool TEXT NOT NULL,args TEXT NOT NULL,status TEXT NOT NULL,nonce TEXT NOT NULL,deadline INTEGER NOT NULL,result TEXT); CREATE INDEX IF NOT EXISTS local_queue ON local_requests(device_id,status);")?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS local_request_progress(request_id TEXT PRIMARY KEY REFERENCES local_requests(id),phase TEXT NOT NULL,seen INTEGER NOT NULL,changed INTEGER NOT NULL);")?;
    Ok(())
}
pub fn enabled(app: &App, bot: &Bot) -> bool {
    bot.profile.local_access
        && !bot.profile.local_device_id.is_empty()
        && app
            .db
            .setting("general")
            .ok()
            .flatten()
            .is_some_and(|v| v["local_access"] == true)
}
pub fn allows_device(bot: &Bot, device: &str) -> bool {
    !device.is_empty()
        && (bot.profile.local_device_id == "*" || bot.profile.local_device_id == device)
}
/// Read configuration and heartbeat metadata only. This never grants access or
/// queues a desktop operation, and remains available when local tools are gated.
pub fn status(app: &App, bot_id: &str) -> Result<Value> {
    let bot = app.db.bot(bot_id)?;
    let global = app.db.setting("general")?.unwrap_or_default()["local_access"] == true;
    let devices = {
        let c = app.db.0.lock().unwrap();
        let mut q = c.prepare("SELECT d.id,COALESCE(n.name,d.name),d.mode,d.seen FROM local_devices d LEFT JOIN local_device_names n ON n.device_id=d.id ORDER BY 2")?;
        q.query_map([], |r| Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"mode":r.get::<_,String>(2)?,"online":r.get::<_,i64>(3)?>db::now()-20})))?
            .collect::<std::result::Result<Vec<Value>, _>>()?
    };
    let all = bot.profile.local_device_id == "*";
    let selected = devices
        .iter()
        .find(|d| d["id"] == bot.profile.local_device_id);
    let mut blockers = Vec::new();
    if !global {
        blockers.push(json!({"code":"global_off","action":"Open Settings > Computer and turn on Local access."}));
    }
    if bot.profile.local_device_id.is_empty() {
        blockers.push(json!({"code":"desktop_not_selected","action":format!("Open {} > Settings and choose a Local desktop.",bot.name)}));
    }
    if !bot.profile.local_access {
        blockers.push(json!({"code":"bot_off","action":format!("In {} > Settings, turn on Local access for this bot.",bot.name)}));
    }
    if all {
        if !devices
            .iter()
            .any(|d| d["online"] == true && d["mode"] != "off")
        {
            blockers.push(json!({"code":"desktops_offline","action":"Open Kindred in this workspace on a paired desktop with local access enabled. All paired desktops remains selected."}));
        }
    } else if let Some(d) = selected {
        if d["online"] != true {
            blockers.push(json!({"code":"desktop_offline","action":"The saved desktop is offline. Open Kindred there in this workspace; the assignment is retained and another computer will not be substituted."}));
        }
        if d["mode"] == "off" {
            blockers.push(json!({"code":"desktop_permission_off","action":"On the selected desktop, open Settings > Computer > Desktop permissions and choose the access you want to grant."}));
        }
    } else if !bot.profile.local_device_id.is_empty() {
        blockers.push(json!({"code":"desktop_unavailable","action":"The selected desktop is not registered in this workspace. Open it in Kindred or select an available Local desktop in the bot settings."}));
    } else if devices.is_empty() {
        blockers.push(json!({"code":"no_desktops","action":"Open the Kindred desktop app in this workspace and configure Desktop permissions in Settings > Computer."}));
    }
    let ready = blockers.is_empty();
    Ok(
        json!({"supported":true,"global_enabled":global,"bot_enabled":bot.profile.local_access,
        "device_selected":!bot.profile.local_device_id.is_empty(),"selected_device":selected,"selected_device_id":bot.profile.local_device_id,"selection":if all {"all"} else {"specific"},
        "available_desktops":devices,"ready":ready,"blockers":blockers,
        "commands_available":ready && devices.iter().any(|d| (all || d["id"] == bot.profile.local_device_id) && d["online"] == true && (d["mode"] == "ask" || d["mode"] == "full")),
        "routing":"A specific saved desktop remains assigned while offline, across client switches and restarts. All paired desktops permits each registered desktop in this workspace, including ones paired later. With All selected, every local tool call must specify a device_id from available_desktops; a call targets exactly one desktop and is never broadcast or automatically failed over. Respect the user's named machine even when others are online.",
        "guidance":"Local desktop access is a supported Kindred feature, separate from Marketplace connectors and the shared Linux VM. These are current settings, not a permanent architectural limit. Only the user can enable access. A saved question answer does not change permission settings. Workspace mode restricts files to the Kindred workspace and does not allow commands; ask/full modes still enforce desktop permissions. Tools are selected at turn start: if access is now ready but local tools are absent from this turn, ask the user to send Continue to start a fresh turn. Verify WSL availability using the paired Windows desktop's local tools; guest_exec and computer tools target the separate Linux VM."}),
    )
}
pub async fn bot_status(
    State(app): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(status(&app, &id)?))
}
pub fn specs() -> Vec<Value> {
    [
        ("local_skill_scan", "Find portable commands, prompt templates and skills on the paired desktop. First confirm the intended desktop, WSL distribution if applicable, and work/project folder. Empty path searches only known home .claude, .codex, .agents and .pi/agent locations. Supply the confirmed workspace or context/skills folder for project-local workflows. This is a bounded scan, not a complete nested-project inventory. On Windows, named registered local WSL distributions are supported through their wsl.localhost paths. Existing local permissions apply. Only discovers paths, never executes or imports.", json!({"path":{"type":"string"}}), vec!["path"]),
        ("local_list", "List files on the paired desktop. Relative paths start in its Kindred workspace.", json!({"path":{"type":"string"}}), vec!["path"]),
        ("local_read", "Read a UTF-8 file on the paired desktop, up to 1 MiB. Contents go to your model provider.", json!({"path":{"type":"string"}}), vec!["path"]),
        ("local_write", "Create or replace a UTF-8 file on the paired desktop, up to 1 MiB. Parent must already exist.", json!({"path":{"type":"string"},"text":{"type":"string","maxLength":1048576}}), vec!["path","text"]),
        ("local_mkdir", "Create one directory on the paired desktop. Parent must already exist.", json!({"path":{"type":"string"}}), vec!["path"]),
        ("local_exec", "Run a command on the paired desktop (PowerShell on Windows). Commands can access the OS account beyond their working folder. Requires desktop permission; workspace-only mode denies commands. Use action_scope=external for remote changes, sending, publishing, purchases or uncertain effects.", json!({"path":{"type":"string"},"command":{"type":"string","maxLength":16000},"action_scope":{"type":"string","enum":["routine_vm","external"]}}), vec!["path","command","action_scope"]),
    ].into_iter().map(|(name,description,mut properties,required)|{
        if name == "local_exec" { properties["background"]=json!({"type":"boolean","description":"Start a managed background command; use command_wait to continue when finished without AI polling."}); properties["title"]=json!({"type":"string","maxLength":120}); properties["max_seconds"]=json!({"type":"integer","minimum":1,"maximum":604800,"description":"Background duration limit; default one day"}); properties["timeout_seconds"] = json!({"type":"integer","minimum":1,"maximum":120,"description":"Maximum command duration, default 60s. Use a short bound for availability probes; on timeout report the observed failure, do not repeat the same stalled probe."}); }
        properties["device_id"]=json!({"type":"string","description":"Exact desktop ID from local_access_status. Required when All paired desktops is selected; otherwise omit or use the saved desktop ID. Never substitute another machine for a named offline target."});
        json!({"type":"function","name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
    }).collect()
}
pub async fn devices(State(app): State<Shared>) -> Result<Json<Value>, crate::web::Error> {
    let c = app.db.0.lock().unwrap();
    let mut q = c.prepare("SELECT d.id,COALESCE(n.name,d.name),d.mode,d.seen FROM local_devices d LEFT JOIN local_device_names n ON n.device_id=d.id ORDER BY 2")?;
    let rows=q.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"mode":r.get::<_,String>(2)?,"online":r.get::<_,i64>(3)?>db::now()-20})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(Json(json!({"devices":rows})))
}
pub async fn rename_device(
    State(app): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(rename_device_inner(&app, &id, &v)?))
}
fn rename_device_inner(app: &App, id: &str, v: &Value) -> Result<Value> {
    let name = v["name"].as_str().context("Missing computer name")?.trim();
    ensure!(
        !name.is_empty() && name.len() <= 100 && !name.chars().any(char::is_control),
        "Use a computer name between 1 and 100 characters"
    );
    ensure!(
        v.as_object().is_some_and(|o| o.len() == 1),
        "Only the computer name can be changed here. Grant execution permissions on that desktop."
    );
    let c = app.db.0.lock().unwrap();
    ensure!(
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_devices WHERE id=?)",
            [&id],
            |r| r.get::<_, bool>(0)
        )?,
        "This computer is not saved in this workspace"
    );
    c.execute("INSERT INTO local_device_names VALUES(?,?) ON CONFLICT(device_id) DO UPDATE SET name=excluded.name", params![id,name])?;
    Ok(json!({"id":id,"name":name}))
}
// This endpoint also needs the ordinary server bearer token. The independent secret
// prevents another linked desktop from claiming work for an existing device ID.
pub async fn poll(
    State(app): State<Shared>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, crate::web::Error> {
    poll_inner(&app, &v).map_err(Into::into)
}
fn poll_inner(app: &App, v: &Value) -> Result<Json<Value>> {
    ensure!(
        app.config.vm.managed_id.is_empty()
            || v["profile_id"].as_str() == Some(app.config.vm.managed_id.as_str()),
        "Update Kindred desktop and reopen this profile before enabling local access."
    );
    let id = v["id"].as_str().context("Missing desktop ID")?;
    let secret = v["secret"].as_str().context("Missing desktop credential")?;
    let mode = v["mode"].as_str().context("Missing desktop mode")?;
    let name = v["name"].as_str().unwrap_or("Desktop");
    ensure!(
        uuid::Uuid::parse_str(id).is_ok()
            && secret.len() == 72
            && name.len() <= 100
            && !name.chars().any(char::is_control)
            && matches!(mode, "off" | "workspace" | "ask" | "full"),
        "Invalid desktop registration"
    );
    {
        let c = app.db.0.lock().unwrap();
        let old: Option<String> = c
            .query_row("SELECT secret FROM local_devices WHERE id=?", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(old) = old {
            ensure!(
                bool::from(old.as_bytes().ct_eq(secret.as_bytes())),
                "Desktop credential mismatch"
            );
        } else {
            c.execute(
                "INSERT INTO local_devices VALUES(?,?,?,?,?)",
                params![id, secret, name, mode, db::now()],
            )?;
        }
        c.execute(
            "UPDATE local_devices SET name=?,mode=?,seen=? WHERE id=?",
            params![name, mode, db::now(), id],
        )?;
        if let Some(progress) = v.get("progress").filter(|p| p.is_object()) {
            let phase = progress["phase"].as_str().unwrap_or("");
            ensure!(
                matches!(phase, "starting" | "awaiting_approval" | "running"),
                "Invalid desktop progress"
            );
            c.execute("INSERT INTO local_request_progress(request_id,phase,seen,changed) SELECT id,?1,?2,?2 FROM local_requests WHERE id=?3 AND device_id=?4 AND status='claimed' ON CONFLICT(request_id) DO UPDATE SET changed=CASE WHEN local_request_progress.phase=excluded.phase THEN local_request_progress.changed ELSE excluded.changed END,phase=excluded.phase,seen=excluded.seen",params![phase,db::now(),progress["id"].as_str().unwrap_or(""),id])?;
        }
        if let Some(receipt) = v.get("receipt") {
            let result = serde_json::to_string(&receipt["result"])?;
            let tool:Option<String>=c.query_row("SELECT tool FROM local_requests WHERE id=? AND device_id=? AND nonce=? AND status='claimed'",params![receipt["id"].as_str().unwrap_or(""),id,receipt["nonce"].as_str().unwrap_or("")],|r|r.get(0)).optional()?;
            let limit = if matches!(
                tool.as_deref(),
                Some("local_skill_bundle" | "local_workspace_bundle")
            ) {
                12 * 1024 * 1024
            } else {
                1_200_000
            };
            ensure!(result.len() <= limit, "Local result is too large");
            c.execute("UPDATE local_requests SET result=?,status='done' WHERE id=? AND device_id=? AND nonce=? AND status='claimed'",params![result,receipt["id"].as_str().unwrap_or(""),id,receipt["nonce"].as_str().unwrap_or("")])?;
        }
        c.execute("UPDATE local_requests SET status='expired' WHERE status IN ('queued','claimed') AND (deadline<=? OR NOT EXISTS(SELECT 1 FROM runs WHERE runs.id=local_requests.run_id AND runs.status='running') OR NOT EXISTS(SELECT 1 FROM settings WHERE key='general' AND json_extract(value,'$.local_access')=1) OR NOT EXISTS(SELECT 1 FROM bots WHERE bots.id=local_requests.bot_id AND json_extract(profile,'$.local_access')=1 AND (json_extract(profile,'$.local_device_id')=local_requests.device_id OR json_extract(profile,'$.local_device_id')='*')))",[db::now()])?;
    }
    let commands = crate::command_jobs::desktop_poll(app, id, v)?;
    let active: Option<String> = app
        .db
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT id FROM local_requests WHERE device_id=? AND status='claimed' AND deadline>?",
            params![id, db::now()],
            |r| r.get(0),
        )
        .optional()?;
    if v["busy"] == true || mode == "off" {
        return Ok(Json(
            json!({"active":active,"request":null,"commands":commands}),
        ));
    }
    let candidate:Option<(String,String,String,String,i64)>=app.db.0.lock().unwrap().query_row("SELECT id,bot_id,tool,args,deadline FROM local_requests WHERE device_id=? AND status='queued' ORDER BY rowid LIMIT 1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    if let Some((request, bot_id, tool, args, deadline)) = candidate {
        let bot = app.db.bot(&bot_id)?;
        if enabled(&app, &bot) && allows_device(&bot, id) && active.is_none() {
            let nonce = db::id();
            let changed=app.db.0.lock().unwrap().execute("UPDATE local_requests SET status='claimed',nonce=? WHERE id=? AND status='queued' AND deadline>? AND EXISTS(SELECT 1 FROM runs WHERE runs.id=local_requests.run_id AND status='running') AND NOT EXISTS(SELECT 1 FROM local_requests AS other WHERE other.device_id=local_requests.device_id AND other.status='claimed')",params![nonce,request,db::now()])?;
            if changed == 1 {
                return Ok(Json(
                    json!({"commands":commands,"active":request,"request":{"id":request,"nonce":nonce,"bot":bot.name,"tool":tool,"args":serde_json::from_str::<Value>(&args)?,"deadline":deadline}}),
                ));
            }
        }
    }
    Ok(Json(
        json!({"active":active,"request":null,"commands":commands}),
    ))
}
pub fn progress(app: &App, run_id: &str) -> Result<Option<Value>> {
    Ok(app.db.0.lock().unwrap().query_row(
        "SELECT COALESCE(n.name,d.name),r.status,p.phase,p.seen,p.changed,d.seen FROM local_requests r JOIN local_devices d ON d.id=r.device_id LEFT JOIN local_device_names n ON n.device_id=d.id LEFT JOIN local_request_progress p ON p.request_id=r.id WHERE r.run_id=? AND r.status IN ('queued','claimed') ORDER BY r.rowid DESC LIMIT 1",
        [run_id], |r| Ok(json!({"desktop":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"phase":r.get::<_,Option<String>>(2)?,"seen":r.get::<_,Option<i64>>(3)?,"changed":r.get::<_,Option<i64>>(4)?,"desktop_seen":r.get::<_,i64>(5)?}))).optional()?)
}
pub fn selected_device(app: &App, bot: &Bot, args: &Value) -> Result<String> {
    let current = app.db.bot(&bot.id)?;
    ensure!(
        enabled(app, &current),
        "Local access is disabled globally or for this bot"
    );
    let device = if current.profile.local_device_id == "*" {
        args["device_id"].as_str().filter(|id| uuid::Uuid::parse_str(id).is_ok())
            .context("All paired desktops is selected. Call local_access_status and specify one exact device_id for this operation; do not broadcast or substitute a different computer.")?
    } else {
        if let Some(requested) = args["device_id"].as_str() {
            ensure!(requested == current.profile.local_device_id, "This bot is assigned to a different desktop. Its saved assignment cannot be overridden by a tool call.");
        }
        &current.profile.local_device_id
    }.to_owned();
    Ok(device)
}
pub async fn call(app: &App, bot: &Bot, run: &Run, tool: &str, args: Value) -> Result<Value> {
    let device = selected_device(app, bot, &args)?;
    let available: bool = app.db.0.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_devices WHERE id=? AND mode!='off' AND seen>?)",
        params![device, db::now() - 20],
        |r| r.get(0),
    )?;
    ensure!(
        available,
        "The paired desktop is offline or local access is off. Open Kindred on that desktop; do not substitute cloud files"
    );
    let encoded = serde_json::to_string(&args)?;
    ensure!(encoded.len() <= 1_100_000, "Local request is too large");
    let id = db::id();
    let deadline = db::now() + 300;
    app.db.0.lock().unwrap().execute(
        "INSERT INTO local_requests VALUES(?,?,?,?,?,?,'queued','',?,NULL)",
        params![id, run.id, bot.id, device, tool, encoded, deadline],
    )?;
    loop {
        let current = app.db.bot(&bot.id)?;
        if app.db.cancelled(&run.id)
            || !enabled(app, &current)
            || !allows_device(&current, &device)
            || db::now() >= deadline
        {
            app.db.0.lock().unwrap().execute("UPDATE local_requests SET status='expired' WHERE id=? AND status IN ('queued','claimed')",[&id])?;
            anyhow::bail!(
                "Local operation stopped or expired. It will not be replayed; verify any already-started effects before retrying"
            );
        }
        let (status, result): (String, Option<String>) = app.db.0.lock().unwrap().query_row(
            "SELECT status,result FROM local_requests WHERE id=?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if status == "done" {
            let mut receipt: Value =
                serde_json::from_str(&result.context("Missing local receipt")?)?;
            // Stamp the server-selected source, never trust a desktop-supplied identity.
            if let Some(object) = receipt.as_object_mut() {
                object.insert("kindred_device_id".into(), json!(device));
            }
            return Ok(receipt);
        }
        ensure!(
            status != "expired",
            "Local operation expired; it will not be replayed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn all_desktops_requires_one_target_and_never_fails_over() {
        let app = crate::tests::app();
        let a = registration();
        let mut b = registration();
        b["name"] = json!("Second desktop");
        b["mode"] = json!("full");
        let (mut bot, run) = prepare(&app, &a);
        let _ = poll_inner(&app, &b).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE local_devices SET seen=0 WHERE id=?",
                [a["id"].as_str().unwrap()],
            )
            .unwrap();
        assert!(
            call(&app, &bot, &run, "local_read", json!({"path":"example"}))
                .await
                .unwrap_err()
                .to_string()
                .contains("offline")
        );
        assert_eq!(
            app.db.bot(&bot.id).unwrap().profile.local_device_id,
            a["id"]
        );
        bot.profile.local_device_id = "*".into();
        app.db.save_bot(&bot).unwrap();
        assert_eq!(status(&app, &bot.id).unwrap()["ready"], true);
        assert!(
            call(&app, &bot, &run, "local_read", json!({"path":"example"}))
                .await
                .unwrap_err()
                .to_string()
                .contains("exact device_id")
        );
        assert!(
            call(
                &app,
                &bot,
                &run,
                "local_read",
                json!({"path":"example","device_id":a["id"]})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("offline")
        );
        assert_eq!(
            app.db
                .0
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM local_requests", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let task_app = app.clone();
        let task_bot = bot.clone();
        let task_run = run.clone();
        let target = b["id"].clone();
        let task = tokio::spawn(async move {
            call(
                &task_app,
                &task_bot,
                &task_run,
                "local_read",
                json!({"path":"example","device_id":target}),
            )
            .await
        });
        let request = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let p = poll_inner(&app, &b).unwrap().0;
                if p["request"].is_object() {
                    break p["request"].clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(poll_inner(&app, &a).unwrap().0["request"], Value::Null);
        let mut wrong = a.clone();
        wrong["progress"] = json!({"id":request["id"],"phase":"running"});
        let _ = poll_inner(&app, &wrong).unwrap();
        assert_eq!(
            progress(&app, &run.id).unwrap().unwrap()["phase"],
            Value::Null
        );
        assert_eq!(progress(&app, &run.id).unwrap().unwrap()["desktop"], b["name"]);
        rename_device_inner(&app, b["id"].as_str().unwrap(), &json!({"name":"linux-client"})).unwrap();
        assert_eq!(progress(&app, &run.id).unwrap().unwrap()["desktop"], "linux-client");
        let mut waiting = b.clone();
        waiting["progress"] = json!({"id":request["id"],"phase":"awaiting_approval"});
        let _ = poll_inner(&app, &waiting).unwrap();
        assert_eq!(
            progress(&app, &run.id).unwrap().unwrap()["phase"],
            "awaiting_approval"
        );
        let mut done = b.clone();
        done["receipt"] = json!({"id":request["id"],"nonce":request["nonce"],"result":{"text":"Second desktop result"}});
        let _ = poll_inner(&app, &done).unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap()["text"],
            "Second desktop result"
        );
        assert!(progress(&app, &run.id).unwrap().is_none());
        assert_eq!(app.db.bot(&bot.id).unwrap().profile.local_device_id, "*");
        bot.profile.local_device_id = a["id"].as_str().unwrap().into();
        app.db.save_bot(&bot).unwrap();
        assert!(
            call(
                &app,
                &bot,
                &run,
                "local_read",
                json!({"path":"example","device_id":b["id"]})
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot be overridden")
        );
    }
    #[tokio::test]
    async fn status_is_available_while_disabled_and_does_not_grant_access() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "claude-code");
        let id = app
            .db
            .queue(&bot.id, "I enabled desktop access", 0)
            .unwrap();
        let run = app.db.run(&id).unwrap();
        let mut registration = registration();
        registration["mode"] = json!("full");
        let _ = poll_inner(&app, &registration).unwrap();
        app.db
            .save_setting("general", &json!({"local_access":true}))
            .unwrap();
        let specs = crate::runtime::tool_specs_for(&app, &bot);
        assert!(specs.iter().any(|t| t["name"] == "local_access_status"));
        assert!(!specs.iter().any(|t| t["name"] == "local_read"));
        let result = crate::runtime::call_tool(&app, &bot, &run, "local_access_status", json!({}))
            .await
            .unwrap();
        assert_ne!(result["failed"], true);
        let state: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
        assert_eq!(state["global_enabled"], true);
        assert_eq!(state["bot_enabled"], false);
        assert_eq!(state["device_selected"], false);
        assert_eq!(state["ready"], false);
        assert_eq!(
            state["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["code"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["desktop_not_selected", "bot_off"]
        );
        assert_eq!(state["available_desktops"][0]["online"], true);
        assert!(
            !state
                .to_string()
                .contains(registration["secret"].as_str().unwrap())
        );
        assert!(
            call(
                &app,
                &bot,
                &run,
                "local_read",
                json!({"path":"private.txt"})
            )
            .await
            .is_err()
        );
        let mut current = bot.clone();
        current.profile.local_access = true;
        current.profile.local_device_id = registration["id"].as_str().unwrap().into();
        app.db.save_bot(&current).unwrap();
        // Even a run with an older bot snapshot sees the actual current gates.
        let result = crate::runtime::call_tool(&app, &bot, &run, "local_access_status", json!({}))
            .await
            .unwrap();
        let state: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
        assert_eq!(state["ready"], true);
        assert_eq!(state["commands_available"], true);
        let instructions = crate::runtime::instructions(&app, &bot, &run).unwrap();
        assert!(instructions.contains("fresh configuration"));
        assert!(instructions.contains("\"ready\":true"));
        assert!(instructions.contains("fresh turn"));
        assert!(
            crate::runtime::tool_specs_for(&app, &current)
                .iter()
                .any(|t| t["name"] == "local_skill_scan")
        );
        let c = app.db.0.lock().unwrap();
        for table in ["local_requests", "approvals"] {
            let count: i64 = c
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
    }
    #[tokio::test]
    async fn status_endpoint_reports_permission_and_device_gates_without_secrets() {
        let app = crate::tests::app();
        let mut registration = registration();
        let (bot, _) = prepare(&app, &registration);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/bots/{}/local-access",
            listener.local_addr().unwrap(),
            bot.id
        );
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        assert_eq!(
            client.get(&url).send().await.unwrap().status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        let state: Value = client
            .get(&url)
            .bearer_auth(&app.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(state["ready"], true);
        assert_eq!(state["commands_available"], false);
        assert!(
            !state
                .to_string()
                .contains(registration["secret"].as_str().unwrap())
        );
        registration["mode"] = json!("off");
        let _ = poll_inner(&app, &registration).unwrap();
        assert_eq!(
            status(&app, &bot.id).unwrap()["blockers"][0]["code"],
            "desktop_permission_off"
        );
        registration["mode"] = json!("full");
        let _ = poll_inner(&app, &registration).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute("UPDATE local_devices SET seen=0", [])
            .unwrap();
        assert_eq!(
            status(&app, &bot.id).unwrap()["blockers"][0]["code"],
            "desktop_offline"
        );
        app.db
            .0
            .lock()
            .unwrap()
            .execute("DELETE FROM local_devices", [])
            .unwrap();
        assert_eq!(
            status(&app, &bot.id).unwrap()["blockers"][0]["code"],
            "desktop_unavailable"
        );
        app.db
            .save_setting("general", &json!({"local_access":false}))
            .unwrap();
        assert_eq!(
            status(&app, &bot.id).unwrap()["blockers"][0]["code"],
            "global_off"
        );
        server.abort();
    }
    fn registration() -> Value {
        json!({"id":db::id(),"secret":format!("{}{}",db::id(),db::id()),"name":"Fixture desktop","mode":"workspace"})
    }
    fn prepare(app: &App, registration: &Value) -> (Bot, Run) {
        let _ = poll_inner(app, registration).unwrap();
        let mut bot = crate::tests::bot(&app.db, "codex");
        bot.profile.local_access = true;
        bot.profile.local_device_id = registration["id"].as_str().unwrap().into();
        app.db.save_bot(&bot).unwrap();
        app.db
            .save_setting("general", &json!({"local_access":true}))
            .unwrap();
        app.db.queue(&bot.id, "local fixture", 0).unwrap();
        (bot, app.db.claim().unwrap().unwrap())
    }
    async fn queued(app: &App) {
        for _ in 0..50 {
            let n: i64 = app
                .db
                .0
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM local_requests", [], |r| r.get(0))
                .unwrap();
            if n > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("request was not queued")
    }
    #[tokio::test]
    async fn bot_import_reviews_then_saves_a_device_bound_package_without_execution() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let app = crate::tests::app();
        let mut registration = registration();
        let (bot, run) = prepare(&app, &registration);
        let package = json!({"entry":"SKILL.md","name":"desktop-review","source":"fixture/skills/desktop-review/SKILL.md","files":{"SKILL.md":STANDARD.encode("Review $ARGUMENTS using references.md"),"references.md":STANDARD.encode("Fixture reference")}});
        let mut fingerprint = Value::Null;
        for action in ["preview", "import"] {
            let (a, b, r, hash) = (app.clone(), bot.clone(), run.clone(), fingerprint.clone());
            let task = tokio::spawn(async move {
                crate::runtime::call_tool(&a,&b,&r,"skill_import_local",json!({"path":"fixture/skills/desktop-review/SKILL.md","action":action,"expected_hash":hash})).await.unwrap()
            });
            let mut claimed = Value::Null;
            for _ in 0..100 {
                let reply = poll_inner(&app, &registration).unwrap().0;
                if reply["request"].is_object() {
                    claimed = reply["request"].clone();
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(claimed["tool"], "local_skill_bundle");
            registration["receipt"] =
                json!({"id":claimed["id"],"nonce":claimed["nonce"],"result":package});
            poll_inner(&app, &registration).unwrap();
            registration.as_object_mut().unwrap().remove("receipt");
            let result = task.await.unwrap();
            assert_ne!(result["failed"], true, "{result}");
            let body: Value = serde_json::from_str(result["text"].as_str().unwrap()).unwrap();
            fingerprint = body["import"]["hash"].clone();
            assert_eq!(
                app.db.skills().unwrap().len(),
                if action == "preview" { 0 } else { 1 }
            );
        }
        assert!(
            app.db
                .commands()
                .unwrap()
                .iter()
                .any(|c| c.name == "desktop-review")
        );
        assert!(
            !app.db
                .events(&run.id)
                .unwrap()
                .iter()
                .any(|e| e["body"]["tool"] == "guest_exec")
        );
    }
    #[tokio::test]
    async fn workspace_import_reads_the_exact_desktop_and_sync_does_not_fail_over() {
        let app = crate::tests::app();
        let mut registration = registration();
        let (bot, run) = prepare(&app, &registration);
        let (a, b, r, device) = (
            app.clone(),
            bot.clone(),
            run.clone(),
            registration["id"].clone(),
        );
        let task = tokio::spawn(async move {
            crate::runtime::call_tool(
                &a,
                &b,
                &r,
                "workspace_import_start",
                json!({"name":"Harold","path":"/home/fixture/project","device_id":device}),
            )
            .await
            .unwrap()
        });
        let mut claimed = Value::Null;
        for _ in 0..100 {
            let reply = poll_inner(&app, &registration).unwrap().0;
            if reply["request"].is_object() {
                claimed = reply["request"].clone();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(claimed["tool"], "local_workspace_bundle");
        assert_eq!(claimed["args"]["path"], "/home/fixture/project");
        registration["receipt"] = json!({"id":claimed["id"],"nonce":claimed["nonce"],"result":{"root":"/home/fixture/project","documents":[{"path":"AGENTS.md","text":"Project guidance"}],"packages":[],"warnings":[],"kindred_device_id":"spoofed"}});
        let _ = poll_inner(&app, &registration).unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(reply["failed"], true, "{reply}");
        let body: Value = serde_json::from_str(reply["text"].as_str().unwrap()).unwrap();
        let id = body["import_id"].as_str().unwrap();
        crate::workspace_import::draft(&app.db,&bot,&run,&json!({"import_id":id,"name":"Harold","instructions":"Kindred guidance","memory":"Project facts","workflows":[]})).unwrap();
        let review = crate::workspace_import::get(&app.db, id).unwrap();
        let created = crate::workspace_import::apply(
            &app.db,
            id,
            &json!({"revision":review["revision"],"local_access":true}),
        )
        .unwrap();
        let new_bot = app.db.bot(created["bot_id"].as_str().unwrap()).unwrap();
        assert!(new_bot.profile.local_access);
        assert_eq!(new_bot.profile.local_device_id, registration["id"]);
        let origin = crate::workspace_import::origin(&app.db, &new_bot.id).unwrap();
        assert_eq!(origin["source"]["device_id"], registration["id"]);
        assert_eq!(origin["source"]["path"], "/home/fixture/project");
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE local_devices SET seen=0 WHERE id=?",
                [registration["id"].as_str().unwrap()],
            )
            .unwrap();
        let alternate = self::registration();
        let _ = poll_inner(&app, &alternate).unwrap();
        let mut new_bot = new_bot;
        new_bot.profile.local_device_id = "*".into();
        app.db.save_bot(&new_bot).unwrap();
        app.db.queue(&new_bot.id, "Sync origin", 0).unwrap();
        let sync = app.db.claim_bot(&new_bot.id).unwrap().unwrap();
        let reply =
            crate::runtime::call_tool(&app, &new_bot, &sync, "workspace_sync_prepare", json!({}))
                .await
                .unwrap();
        assert_eq!(reply["failed"], true);
        assert!(reply["text"].as_str().unwrap().contains("offline"));
        assert_eq!(
            app.db
                .0
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM local_requests WHERE device_id=?",
                    [alternate["id"].as_str().unwrap()],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn desktop_binding_and_receipts_are_single_use() {
        let app = crate::tests::app();
        let mut registration = registration();
        let (bot, run) = prepare(&app, &registration);
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            call(
                &a,
                &b,
                &r,
                "local_write",
                json!({"path":"test.txt","text":"test"}),
            )
            .await
        });
        queued(&app).await;
        let other = self::registration();
        assert!(poll_inner(&app, &other).unwrap().0["request"].is_null());
        let mut stolen = registration.clone();
        stolen["secret"] = json!(format!("{}{}", db::id(), db::id()));
        assert!(poll_inner(&app, &stolen).is_err());
        let request = poll_inner(&app, &registration).unwrap().0["request"].clone();
        assert_eq!(request["tool"], "local_write");
        assert!(poll_inner(&app, &registration).unwrap().0["request"].is_null());
        registration["receipt"] =
            json!({"id":request["id"],"nonce":"wrong","result":{"text":"forged"}});
        let _ = poll_inner(&app, &registration).unwrap();
        assert!(!task.is_finished());
        registration["receipt"]["nonce"] = request["nonce"].clone();
        registration["receipt"]["result"] = json!({"text":"written"});
        let _ = poll_inner(&app, &registration).unwrap();
        assert_eq!(task.await.unwrap().unwrap()["text"], "written");
        registration["receipt"]["result"] = json!({"text":"duplicate"});
        assert!(poll_inner(&app, &registration).unwrap().0["request"].is_null());
        let saved: String = app
            .db
            .0
            .lock()
            .unwrap()
            .query_row("SELECT result FROM local_requests", [], |r| r.get(0))
            .unwrap();
        assert!(saved.contains("written"));
        assert!(!saved.contains("duplicate"));
    }
    #[tokio::test]
    async fn concurrent_desktops_serialize_locally_and_cancel_independently() {
        let app = crate::tests::app();
        let mut first = registration();
        first["mode"] = json!("full");
        let mut second = registration();
        second["mode"] = json!("full");
        let one = prepare(&app, &first);
        let two = prepare(&app, &first);
        let three = prepare(&app, &second);
        let mut tasks = Vec::new();
        for (index, (bot, run)) in [one.clone(), two.clone(), three.clone()]
            .into_iter()
            .enumerate()
        {
            let app = app.clone();
            tasks.push(tokio::spawn(async move {
                call(
                    &app,
                    &bot,
                    &run,
                    "local_exec",
                    json!({"path":".","command":format!("fixture {index}")}),
                )
                .await
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let count: i64 = app
                    .db
                    .0
                    .lock()
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM local_requests", [], |r| r.get(0))
                    .unwrap();
                if count == 3 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // Select the first desktop's oldest queued operation without relying on task scheduling order.
        let claimed = poll_inner(&app, &first).unwrap().0["request"].clone();
        let peer = poll_inner(&app, &second).unwrap().0["request"].clone();
        assert!(claimed.is_object() && peer.is_object());
        assert_ne!(claimed["id"], peer["id"]);
        assert!(poll_inner(&app, &first).unwrap().0["request"].is_null());
        let cancelled_run: String = app
            .db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT run_id FROM local_requests WHERE id=?",
                [claimed["id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        app.db.cancel(&cancelled_run).unwrap();
        first["busy"] = json!(true);
        let stopping = poll_inner(&app, &first).unwrap().0;
        assert!(stopping["active"].is_null());
        assert!(
            stopping["request"].is_null(),
            "Do not overlap the command still stopping on the desktop"
        );
        assert_eq!(poll_inner(&app, &second).unwrap().0["active"], peer["id"]);
        assert_eq!(app.db.run(&three.1.id).unwrap().status, "running");
        second["receipt"] = json!({"id":peer["id"],"nonce":peer["nonce"],"result":{"text":"independent desktop finished"}});
        assert!(poll_inner(&app, &second).unwrap().0["request"].is_null());
        first["busy"] = json!(false);
        first["receipt"] = json!({"id":claimed["id"],"nonce":claimed["nonce"],"result":{"text":"late cancelled result"}});
        let next = poll_inner(&app, &first).unwrap().0["request"].clone();
        assert!(next.is_object());
        assert_ne!(next["id"], claimed["id"]);
        first["receipt"] = json!({"id":next["id"],"nonce":next["nonce"],"result":{"text":"next serialized command finished"}});
        assert!(poll_inner(&app, &first).unwrap().0["request"].is_null());
        let mut outcomes = Vec::new();
        for task in tasks {
            outcomes.push(
                tokio::time::timeout(std::time::Duration::from_secs(3), task)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_eq!(outcomes.iter().filter(|r| r.is_err()).count(), 1);
        assert_eq!(
            outcomes[2].as_ref().unwrap()["text"],
            "independent desktop finished"
        );
        let c = app.db.0.lock().unwrap();
        let counts: (i64, i64, i64) = c
            .query_row(
                "SELECT COUNT(*),SUM(status='expired'),SUM(status='done') FROM local_requests",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (3, 1, 2));
        let result: Option<String> = c
            .query_row(
                "SELECT result FROM local_requests WHERE id=?",
                [claimed["id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            result.is_none(),
            "A late cancelled receipt cannot become a successful completion"
        );
    }
    #[tokio::test]
    async fn live_disable_expires_claim_without_requeue() {
        let app = crate::tests::app();
        let registration = registration();
        let (bot, run) = prepare(&app, &registration);
        assert!(
            crate::runtime::tool_specs_for(&app, &bot)
                .iter()
                .any(|s| s["name"] == "local_read")
        );
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            call(&a, &b, &r, "local_read", json!({"path":"test.txt"})).await
        });
        queued(&app).await;
        let request = poll_inner(&app, &registration).unwrap().0["request"].clone();
        assert!(!request.is_null());
        app.db
            .save_setting("general", &json!({"local_access":false}))
            .unwrap();
        assert!(task.await.unwrap().is_err());
        let next = poll_inner(&app, &registration).unwrap().0;
        assert!(next["active"].is_null());
        assert!(next["request"].is_null());
        assert!(
            !crate::runtime::tool_specs_for(&app, &bot)
                .iter()
                .any(|s| s["name"] == "local_read")
        );
        assert!(
            call(&app, &bot, &run, "local_read", json!({"path":"test.txt"}))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn defaults_off_offline_clear_and_disabled_before_claim() {
        let app = crate::tests::app();
        let registration = registration();
        let (mut bot, run) = prepare(&app, &registration);
        assert!(!crate::db::BotProfile::default().local_access);
        let (a, b, r) = (app.clone(), bot.clone(), run.clone());
        let task = tokio::spawn(async move {
            call(&a, &b, &r, "local_read", json!({"path":"test.txt"})).await
        });
        queued(&app).await;
        bot.profile.local_access = false;
        app.db.save_bot(&bot).unwrap();
        assert!(poll_inner(&app, &registration).unwrap().0["request"].is_null());
        assert!(task.await.unwrap().is_err());
        bot.profile.local_access = true;
        app.db.save_bot(&bot).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute("UPDATE local_devices SET seen=0", [])
            .unwrap();
        assert!(
            call(&app, &bot, &run, "local_read", json!({"path":"test.txt"}))
                .await
                .unwrap_err()
                .to_string()
                .contains("offline")
        );
    }
}
