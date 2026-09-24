//! Existing bot instructions can only change after a concrete one-time review.
use crate::{
    db::{Bot, Run},
    runtime::{self, App},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
fn target(app: &App, actor: &Bot, args: &Value) -> Result<Bot> {
    let id = runtime::string(args, "bot_id")?;
    let target = app.db.bot(if id == "self" { &actor.id } else { id })?;
    ensure!(
        !target.profile.archived,
        "Choose an active bot (self is supported)"
    );
    Ok(target)
}
pub fn get(app: &App, actor: &Bot, args: &Value) -> Result<Value> {
    let target = target(app, actor, args)?;
    Ok(
        json!({"text":serde_json::to_string(&json!({"bot_id":target.id,"name":target.name,"instructions":target.instructions,"edit_requires":"One-time user Allow, even in Full access"}))?}),
    )
}
pub async fn update(app: &App, actor: &Bot, run: &Run, args: &Value) -> Result<Value> {
    let target = target(app, actor, args)?;
    let before = runtime::string(args, "expected_instructions")?;
    let after = runtime::string(args, "instructions")?;
    ensure!(
        before.len() <= crate::db::BOT_INSTRUCTIONS_MAX_BYTES && after.len() <= crate::db::BOT_INSTRUCTIONS_MAX_BYTES && !after.trim().is_empty(),
        "Instructions must contain text and fit within 32,000 UTF-8 bytes"
    );
    ensure!(
        target.instructions == before,
        "These instructions changed. Read them again and merge the requested change before asking for approval"
    );
    ensure!(
        !app.account_disabled() && app.db.transfer_status()?.is_null(),
        "This workspace is paused"
    );
    if before == after {
        return Ok(
            json!({"text":"The requested instructions are already saved. No change was made."}),
        );
    }
    let review = json!({"bot_id":target.id,"bot_name":target.name,"expected_instructions":before,"instructions":after,"approval_reason":"Editing bot instructions, including your own, requires one-time Allow for this exact change."});
    if !runtime::approve_required(app, actor, run, "bot_instructions_update", &review, true).await?
    {
        return Ok(
            json!({"failed":true,"text":"The user declined this instruction change. Nothing was changed; do not retry or route around their decision."}),
        );
    }
    ensure!(
        !app.db.cancelled(&run.id)
            && !app.account_disabled()
            && app.db.transfer_status()?.is_null(),
        "This task or workspace is no longer active"
    );
    {
        let mut c = app.db.0.lock().unwrap();
        let tx = c.transaction()?;
        ensure!(
            !crate::workspace_transfer::frozen(&tx)?,
            "This workspace is paused"
        );
        let changed=tx.execute("UPDATE bots SET instructions=?1 WHERE id=?2 AND instructions=?3 AND COALESCE(json_extract(profile,'$.archived'),0)=0 AND EXISTS(SELECT 1 FROM runs WHERE id=?4 AND bot_id=?5 AND status='running')",rusqlite::params![after,target.id,before,run.id,actor.id])?;
        ensure!(
            changed == 1,
            "The task, teammate or instructions changed during review. Nothing was overwritten; read the current state before proposing a new change"
        );
        tx.commit()?;
    }
    Ok(
        json!({"text":serde_json::to_string(&json!({"saved":true,"bot_id":target.id,"name":target.name,"instructions":after,"applies_to":"Future tasks. Running tasks and other bot settings are unchanged."}))?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn full_access_still_requires_each_review_and_preserves_other_fields() {
        review_cases(false).await;
    }
    #[tokio::test]
    async fn self_edit_supports_allow_deny_stale_and_cancel() {
        review_cases(true).await;
    }
    async fn review_cases(self_edit: bool) {
        let app = crate::tests::app();
        let mut actor = crate::tests::bot(&app.db, "codex");
        actor.approval_mode = "full".into();
        app.db.save_bot(&actor).unwrap();
        let mut target = if self_edit {
            actor.clone()
        } else {
            crate::tests::bot(&app.db, "claude-code")
        };
        target.memory = "Private memory remains intact".into();
        app.db.save_bot(&target).unwrap();
        let id = app
            .db
            .queue(&actor.id, "Update the teammate's instructions", 0)
            .unwrap();
        app.db.claim_bot(&actor.id).unwrap();
        let run = app.db.run(&id).unwrap();
        for scenario in ["deny", "allow", "stale", "cancel"] {
            let before = app.db.bot(&target.id).unwrap();
            let args = json!({"bot_id":if self_edit{"self"}else{target.id.as_str()},"expected_instructions":before.instructions,"instructions":format!("Role change: {scenario}")});
            let task = {
                let a = app.clone();
                let b = actor.clone();
                let r = run.clone();
                tokio::spawn(async move {
                    runtime::call_tool(&a, &b, &r, "bot_instructions_update", args)
                        .await
                        .unwrap()
                })
            };
            let review = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if let Some(row) = app.db.approvals().unwrap().first() {
                        break row.clone();
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(review["tool"], "bot_instructions_update");
            assert_eq!(
                app.db.bot(&target.id).unwrap().instructions,
                before.instructions
            );
            assert!(!task.is_finished());
            if scenario == "stale" {
                app.db
                    .save_bot_text(
                        &target.id,
                        "instructions",
                        "User's newer edit",
                        Some(&before.instructions),
                    )
                    .unwrap();
            }
            if scenario == "cancel" {
                app.db.cancel(&run.id).unwrap();
            } else {
                app.db
                    .decide(review["id"].as_str().unwrap(), scenario != "deny")
                    .unwrap();
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap();
            let after = app.db.bot(&target.id).unwrap();
            assert_eq!(result["failed"] == true, scenario != "allow");
            assert_eq!(
                after.instructions,
                match scenario {
                    "allow" => "Role change: allow",
                    "stale" => "User's newer edit",
                    _ => before.instructions.as_str(),
                }
            );
            let mut normalized = after.clone();
            normalized.instructions = before.instructions.clone();
            assert_eq!(
                serde_json::to_value(normalized).unwrap(),
                serde_json::to_value(before).unwrap()
            );
        }
        let read = get(&app, &actor, &json!({"bot_id":target.id}))
            .unwrap()
            .to_string();
        assert!(!read.contains(&target.memory));
        assert!(get(&app, &actor, &json!({"bot_id":"self"})).is_ok());
    }
}
