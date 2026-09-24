//! Portable workspace data. Account credentials, provider sessions, desktop grants,
//! and VM disks are deliberately outside this format. Imports only fill empty profiles.
use crate::db::{self, Db};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{
    Connection, OptionalExtension, params,
    types::{Value as Sql, ValueRef},
};
use serde_json::{Value, json};

pub const LIMIT: usize = 256 * 1024 * 1024;
const TABLES: &[&str] = &[
    "bots",
    "chats",
    "runs",
    "events",
    "approvals",
    "skills",
    "screens",
    "routines",
    "attachments",
    "deliverables",
    "chat_messages",
    "message_reactions",
    "chat_completion_receipts",
    "chat_completion_pending",
    "chat_send_receipts",
    "collaboration_requests",
    "chat_reads",
    "bot_drafts",
    "workspace_imports",
    "workspace_origins",
    "uploads",
    "user_message_reactions",
    "message_replies",
    "run_message_sources",
    "run_steering",
    "steering_requests",
    "bot_chat_posts",
    "group_wakeups",
    "run_chat_reads",
    "routine_runs",
    "provider_inbox_routines",
    "provider_inbox_runs",
    "routine_run_requests",
    "run_commands",
    "questions",
    "user_tasks",
    "provider_usage",
    "checklists",
    "reminders",
    "connector_artifacts",
    "visual_panels",
    "workspace_artifacts",
    "workspace_artifact_versions",
    "workspace_artifact_folders",
    "workflow_responses",
    "continuity_notes",
    "continuity_revisions",
];
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS workspace_transfers(id TEXT PRIMARY KEY,state TEXT NOT NULL,package TEXT NOT NULL,destination TEXT NOT NULL DEFAULT '');")?;
    Ok(())
}
pub fn frozen(c: &Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_transfers WHERE state IN ('prepared','moved'))",
        [],
        |r| r.get(0),
    )?)
}
fn columns(c: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(c.prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?)
}
fn encode(value: ValueRef<'_>) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => json!(n),
        ValueRef::Real(n) => json!(n),
        ValueRef::Text(s) => json!(std::str::from_utf8(s)?),
        ValueRef::Blob(b) => json!({"blob":STANDARD.encode(b)}),
    })
}
fn decode(value: &Value) -> Result<Sql> {
    Ok(match value {
        Value::Null => Sql::Null,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Sql::Integer(i)
            } else {
                Sql::Real(
                    n.as_f64()
                        .ok_or_else(|| anyhow::anyhow!("Invalid workspace number"))?,
                )
            }
        }
        Value::String(s) => Sql::Text(s.clone()),
        Value::Object(v) if v.len() == 1 && v.get("blob").is_some_and(Value::is_string) => {
            Sql::Blob(STANDARD.decode(v["blob"].as_str().unwrap())?)
        }
        _ => anyhow::bail!("Invalid workspace field"),
    })
}
fn identifier(id: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok(),
        "Invalid transfer request"
    );
    Ok(())
}
impl Db {
    pub fn transfer_status(&self) -> Result<Value> {
        let c = self.0.lock().unwrap();
        let transfer:Option<(String,String,String)>=c.query_row("SELECT id,state,destination FROM workspace_transfers WHERE state IN ('prepared','moved') LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        Ok(match transfer {
            Some((id, state, destination)) => {
                json!({"id":id,"state":state,"destination":destination})
            }
            None => Value::Null,
        })
    }
    pub fn prepare_transfer(&self, id: &str, name: &str) -> Result<Value> {
        identifier(id)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if let Some((state, package)) = tx
            .query_row(
                "SELECT state,package FROM workspace_transfers WHERE id=?",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            ensure!(
                state == "prepared" || state == "moved",
                "This transfer was cancelled; start a new transfer"
            );
            return Ok(serde_json::from_str(&package)?);
        }
        ensure!(
            !frozen(&tx)?,
            "Resume or cancel the existing workspace transfer first"
        );
        let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')) OR EXISTS(SELECT 1 FROM command_jobs WHERE status IN ('starting','running')) OR EXISTS(SELECT 1 FROM command_waits WHERE continuation='')",[],|r|r.get(0))?;
        ensure!(
            !active,
            "Finish or cancel active tasks before moving this profile"
        );
        ensure!(
            !crate::vm_maintenance::busy(&tx)?,
            "Wait for computer updates before moving this profile"
        );
        let mut tables = serde_json::Map::new();
        for table in TABLES {
            let names = columns(&tx, table)?;
            let mut query = tx.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
            let mut rows = query.query([])?;
            let mut output = Vec::new();
            while let Some(row) = rows.next()? {
                output.push(
                    (0..names.len())
                        .map(|i| encode(row.get_ref(i)?))
                        .collect::<Result<Vec<_>>>()?,
                );
            }
            tables.insert((*table).into(), json!({"columns":names,"rows":output}));
        }
        let general: Option<String> = tx
            .query_row("SELECT value FROM settings WHERE key='general'", [], |r| {
                r.get(0)
            })
            .optional()?;
        let custom: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key='custom_providers'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let custom: Vec<crate::provider_accounts::CustomProvider> = custom
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default();
        for provider in &custom {
            crate::provider_accounts::validate(provider)?;
        }
        let settings = json!({"general":general.map(|s|serde_json::from_str::<Value>(&s)).transpose()?.unwrap_or(json!({})),"custom_providers":custom});
        let package = json!({"format":"kindred-workspace","format_version":1,"id":id,"name":name,"created":db::now(),"tables":tables,"settings":settings});
        let serialized = serde_json::to_string(&package)?;
        ensure!(
            serialized.len() <= LIMIT,
            "This workspace exceeds the 256 MB transfer limit"
        );
        tx.execute(
            "INSERT INTO workspace_transfers(id,state,package) VALUES(?,'prepared',?)",
            params![id, serialized],
        )?;
        tx.commit()?;
        Ok(package)
    }
    pub fn cancel_transfer(&self, id: &str) -> Result<()> {
        identifier(id)?;
        let c = self.0.lock().unwrap();
        ensure!(c.execute("UPDATE workspace_transfers SET state='cancelled',package='' WHERE id=? AND state='prepared'",[id])?==1,"Only a pending transfer can be cancelled");
        Ok(())
    }
    pub fn finish_transfer(&self, id: &str, destination: &str) -> Result<()> {
        identifier(id)?;
        let c = self.0.lock().unwrap();
        ensure!(c.execute("UPDATE workspace_transfers SET state='moved',destination=? WHERE id=? AND (state='prepared' OR (state='moved' AND destination=?))",params![destination,id,destination])?==1,"Transfer unavailable");
        Ok(())
    }
    pub fn import_transfer(&self, package: &Value) -> Result<Value> {
        ensure!(
            package["format"] == "kindred-workspace" && package["format_version"] == 1,
            "Unsupported workspace format; update both servers"
        );
        let id = package["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing transfer ID"))?;
        identifier(id)?;
        let serialized = serde_json::to_string(package)?;
        ensure!(
            serialized.len() <= LIMIT,
            "This workspace exceeds the 256 MB transfer limit"
        );
        let digest = ring::digest::digest(&ring::digest::SHA256, serialized.as_bytes());
        let digest: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let receipt = json!({"id":id,"digest":digest,"state":"imported"});
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        if let Some(old) = tx
            .query_row(
                "SELECT value FROM settings WHERE key='_workspace_import'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            ensure!(
                serde_json::from_str::<Value>(&old)? == receipt,
                "This profile already contains a different workspace"
            );
            return Ok(receipt);
        }
        ensure!(!frozen(&tx)?, "This profile is being transferred");
        for table in TABLES {
            ensure!(
                tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))?
                    == 0,
                "Choose a new, empty profile; existing workspace data is never overwritten"
            );
        }
        let tables = package["tables"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Missing workspace tables"))?;
        ensure!(
            tables.keys().all(|t| TABLES.contains(&t.as_str()))
                && TABLES.iter().all(|t| matches!(
                    *t,
                    "connector_artifacts"
                        | "visual_panels"
                    | "workspace_artifacts"
                    | "workspace_artifact_versions"
                    | "workspace_artifact_folders"
                    | "workflow_responses"
                    | "continuity_notes"
                        | "continuity_revisions"
                        | "chat_completion_pending"
                        | "provider_inbox_routines"
                        | "provider_inbox_runs"
                        | "steering_requests"
                        | "bot_chat_posts"
                        | "group_wakeups"
                        | "run_chat_reads"
                        | "workspace_imports"
                        | "workspace_origins"
                ) || tables.contains_key(*t)),
            "Workspace tables do not match this Kindred version"
        );
        tx.execute_batch("PRAGMA defer_foreign_keys=ON;")?;
        for table in TABLES {
            if matches!(
                *table,
                "connector_artifacts"
                    | "visual_panels"
                    | "workspace_artifacts"
                    | "workspace_artifact_versions"
                    | "workspace_artifact_folders"
                    | "workflow_responses"
                    | "continuity_notes"
                    | "continuity_revisions"
                    | "chat_completion_pending"
                    | "provider_inbox_routines"
                    | "provider_inbox_runs"
                    | "steering_requests"
                    | "bot_chat_posts"
                    | "group_wakeups"
                    | "run_chat_reads"
                    | "workspace_imports"
                    | "workspace_origins"
            ) && !tables.contains_key(*table)
            {
                continue;
            }
            let names = columns(&tx, table)?;
            ensure!(
                tables[*table]["columns"] == json!(names),
                "Workspace schema differs; update both servers before transferring"
            );
            let rows = tables[*table]["rows"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Invalid workspace rows"))?;
            let sql = format!(
                "INSERT INTO {table} VALUES({})",
                vec!["?"; names.len()].join(",")
            );
            let mut statement = tx.prepare(&sql)?;
            for row in rows {
                let row = row
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Invalid workspace row"))?;
                ensure!(row.len() == names.len(), "Invalid workspace field count");
                let values = row.iter().map(decode).collect::<Result<Vec<_>>>()?;
                statement.execute(rusqlite::params_from_iter(values))?;
            }
        }
        ensure!(tx.query_row("SELECT COUNT(*) FROM runs WHERE status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')",[],|r|r.get::<_,i64>(0))?==0,"A workspace with unfinished tasks cannot be imported");
        // Older packages infer folders from document metadata; register those too.
        crate::workspace_artifacts::migrate(&tx)?;
        // A copied policy or pairing can never enable access to a different desktop.
        tx.execute("UPDATE bots SET profile=json_set(profile,'$.local_access',json('false'),'$.local_device_id','')",[])?;
        tx.execute("UPDATE screens SET takeover=0,quiet_until=0", [])?;
        tx.execute("UPDATE routines SET enabled=0", [])?;
        tx.execute("UPDATE connector_artifacts SET status='interrupted',revision=revision+1 WHERE status IN ('preparing','pending','ready','approved','executing')",[])?;
        tx.execute(
            "UPDATE reminders SET status='paused',revision=revision+1 WHERE status='pending'",
            [],
        )?;
        let mut general = package["settings"]["general"].clone();
        ensure!(general.is_object(), "Invalid workspace preferences");
        general["local_access"] = json!(false);
        tx.execute("INSERT INTO settings(key,value) VALUES('general',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&general)?])?;
        let custom: Vec<crate::provider_accounts::CustomProvider> =
            serde_json::from_value(package["settings"]["custom_providers"].clone())?;
        for provider in &custom {
            crate::provider_accounts::validate(provider)?;
        }
        tx.execute("INSERT INTO settings(key,value) VALUES('custom_providers',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&custom)?])?;
        tx.execute(
            "INSERT INTO settings VALUES('_workspace_import',?)",
            [receipt.to_string()],
        )?;
        ensure!(
            !tx.prepare("PRAGMA foreign_key_check")?
                .query([])?
                .next()?
                .is_some(),
            "Workspace references are incomplete"
        );
        tx.commit()?;
        Ok(receipt)
    }
}
