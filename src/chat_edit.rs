//! Exact, one-time approval for group metadata and membership changes.
use crate::{
    db::{Bot, Run},
    runtime::{self, App},
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

pub(crate) fn revision(v: &Value) -> String {
    ring::digest::digest(&ring::digest::SHA256, &serde_json::to_vec(v).unwrap())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(crate) fn patch(before: &Value, args: &Value) -> Result<Value> {
    let mut after = before.clone();
    for key in ["name", "description"] {
        if let Some(value) = args.get(key) {
            let text = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("{key} must be text"))?
                .trim();
            ensure!(
                text.chars().count() <= if key == "name" { 100 } else { 2000 },
                "{key} is too long"
            );
            ensure!(
                key != "name" || (!text.is_empty() && text.len() <= 100),
                "Name must be 1 to 100 UTF-8 bytes"
            );
            after[key] = json!(text);
        }
    }
    if let Some(value) = args.get("members") {
        let mut ids: Vec<String> = serde_json::from_value(value.clone())?;
        ensure!(!ids.is_empty() && ids.len() <= 64, "Choose 1 to 64 members");
        ids.sort();
        ensure!(
            ids.windows(2).all(|w| w[0] != w[1]),
            "Choose distinct members"
        );
        after["members"] = json!(ids);
    }
    Ok(after)
}
pub(crate) fn active(c: &rusqlite::Connection, actor: &Bot, run: &Run) -> Result<()> {
    ensure!(
        !crate::workspace_transfer::frozen(c)?,
        "This workspace is paused"
    );
    let valid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM runs r JOIN bots b ON b.id=r.bot_id WHERE r.id=? AND r.bot_id=? AND r.status='running' AND COALESCE(json_extract(b.profile,'$.archived'),0)=0)",params![run.id,actor.id],|r|r.get(0))?;
    ensure!(valid, "This task or bot is no longer active");
    Ok(())
}
pub(crate) fn review(before: Value, after: Value, revision: String, people: Value) -> Value {
    json!({"chat_id":before["id"],"chat_name":before["name"],"expected_revision":revision,"before":before,"after":after,"people":people})
}
fn local(
    app: &App,
    actor: &Bot,
    run: Option<&Run>,
    args: &Value,
    approved: Option<&Value>,
) -> Result<Value> {
    let id = runtime::string(args, "chat_id")?;
    ensure!(
        !id.starts_with("dm-") && !id.starts_with("server-"),
        "Choose a group chat"
    );
    let mut c = app.db.0.lock().unwrap();
    let tx = c.transaction()?;
    let (name, description, raw, archived): (String, String, String, bool) = tx.query_row(
        "SELECT name,description,members,archived FROM chats WHERE id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut ids: Vec<String> = serde_json::from_str(&raw)?;
    ids.sort();
    ensure!(
        !archived && ids.contains(&actor.id),
        "Choose an active group you belong to"
    );
    let before = json!({"id":id,"name":name,"description":description,"members":ids});
    let rev = revision(&before);
    let people:Vec<Value>=tx.prepare("SELECT id,name FROM bots WHERE COALESCE(json_extract(profile,'$.archived'),0)=0 ORDER BY id")?.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?})))?.collect::<rusqlite::Result<_>>()?;
    if run.is_none() {
        return Ok(json!({"chat":before,"revision":rev,"available_members":people}));
    }
    ensure!(
        args["expected_revision"] == rev,
        "The chat changed. Read it again before proposing changes"
    );
    active(&tx, actor, run.unwrap())?;
    let after = patch(&before, args)?;
    let members: Vec<String> = serde_json::from_value(after["members"].clone())?;
    ensure!(members.len() <= 6, "Local groups allow up to six bots");
    for member in &members {
        ensure!(
            people.iter().any(|p| p["id"] == *member),
            "Choose active bots from this workspace"
        );
    }
    let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE chat_id=?1 AND bot_id NOT IN (SELECT value FROM json_each(?2)) AND id<>?3 AND status IN ('queued','running','awaiting_user','awaiting_approval','cancelling'))",params![id,after["members"].to_string(),run.unwrap().id],|r|r.get(0))?;
    ensure!(
        !busy,
        "Stop the removed members' active work before changing membership"
    );
    let proposal = review(before, after.clone(), rev, json!(people));
    if let Some(approved) = approved {
        ensure!(
            approved == &proposal,
            "The chat or member identities changed during review. Nothing was overwritten"
        );
        active(&tx, actor, run.unwrap())?;
        tx.execute(
            "UPDATE chats SET name=?,description=?,members=? WHERE id=?",
            params![
                after["name"].as_str(),
                after["description"].as_str(),
                after["members"].to_string(),
                id
            ],
        )?;
        tx.commit()?;
    }
    Ok(proposal)
}
fn access(
    app: &App,
    actor: &Bot,
    run: Option<&Run>,
    args: &Value,
    approved: Option<&Value>,
) -> Result<Value> {
    ensure!(!app.account_disabled(), "This account is disabled");
    if runtime::string(args, "chat_id")?.starts_with("server-") {
        let (portal, profile) = app
            .profile_portal
            .get()
            .ok_or_else(|| anyhow::anyhow!("Shared chat service is unavailable"))?;
        let portal = portal
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Shared chat service is unavailable"))?;
        portal.bot_chat_edit(profile, app, actor, run, args, approved)
    } else {
        local(app, actor, run, args, approved)
    }
}
pub fn get(app: &App, actor: &Bot, args: &Value) -> Result<Value> {
    access(app, actor, None, args, None)
}
pub async fn update(app: &App, actor: &Bot, run: &Run, args: &Value) -> Result<Value> {
    let proposed = access(app, actor, Some(run), args, None)?;
    if proposed["before"] == proposed["after"] {
        return Ok(json!({"text":"The chat already has those settings. Nothing changed."}));
    }
    if !runtime::approve_required(app, actor, run, "chat_update", &proposed, true).await? {
        return Ok(
            json!({"failed":true,"text":"The chat edit was declined. Nothing changed; do not retry without a new request."}),
        );
    }
    let saved = access(app, actor, Some(run), args, Some(&proposed))?;
    Ok(json!({"text":serde_json::to_string(&json!({"saved":true,"chat":saved["after"]}))?}))
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(crate) async fn pending(app: &App) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            loop {
                if let Some(a) = app.db.approvals().unwrap().first() {
                    return a.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn chat_edit_requires_approval_and_rejects_denial_stale_and_cancel() {
        for scenario in ["allow", "deny", "stale", "cancel"] {
            let app = crate::tests::app();
            let mut actor = crate::tests::bot(&app.db, "codex");
            actor.approval_mode = "full".into();
            app.db.save_bot(&actor).unwrap();
            let peer = crate::tests::bot(&app.db, "codex");
            let added = crate::tests::bot(&app.db, "codex");
            let chat = crate::chats::Chat {
                id: "group-edit".into(),
                name: "Team".into(),
                description: "Old purpose".into(),
                bot_only: true,
                members: vec![actor.id.clone(), peer.id.clone()],
                archived: false,
                pinned: true,
                last_message: None,
            };
            app.db.save_chat(&chat).unwrap();
            let id = app.db.queue(&actor.id, "Edit the group", 0).unwrap();
            app.db.claim_bot(&actor.id).unwrap();
            let run = app.db.run(&id).unwrap();
            let snapshot = get(&app, &actor, &json!({"chat_id":chat.id})).unwrap();
            let args = json!({"chat_id":chat.id,"expected_revision":snapshot["revision"],"name":"team-updates","description":"One line per completed item.","members":[actor.id,added.id]});
            let a = app.clone();
            let b = actor.clone();
            let r = run.clone();
            let task = tokio::spawn(async move {
                runtime::call_tool(&a, &b, &r, "chat_update", args)
                    .await
                    .unwrap()
            });
            let approval = pending(&app).await;
            assert_eq!(approval["tool"], "chat_update");
            assert_eq!(app.db.chat(&chat.id).unwrap().description, "Old purpose");
            if scenario == "stale" {
                app.db
                    .0
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE chats SET description='Human edit' WHERE id=?",
                        [&chat.id],
                    )
                    .unwrap();
            }
            if scenario == "cancel" {
                app.db.cancel(&id).unwrap();
            } else {
                app.db
                    .decide(approval["id"].as_str().unwrap(), scenario != "deny")
                    .unwrap();
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(4), task)
                .await
                .unwrap()
                .unwrap();
            let saved = app.db.chat(&chat.id).unwrap();
            if scenario == "allow" {
                assert_eq!(saved.name, "team-updates");
                assert!(
                    saved.members.contains(&actor.id)
                        && saved.members.contains(&added.id)
                        && !saved.members.contains(&peer.id)
                );
                assert_ne!(result["failed"], true);
            } else {
                assert_eq!(saved.name, "Team");
                assert_eq!(
                    saved.description,
                    if scenario == "stale" {
                        "Human edit"
                    } else {
                        "Old purpose"
                    }
                );
                assert_eq!(result["failed"], true);
            }
            assert!(saved.bot_only);
            assert!(get(&app, &peer, &json!({"chat_id":format!("dm-{}",actor.id)})).is_err());
        }
    }
}
