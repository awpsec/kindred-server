//! A current-work label, independent of execution and scheduling.
use crate::{
    chats::Chat,
    db::{self, BotProfile},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CurrentTask {
    pub label: String,
    pub revision: u64,
    pub writer_run: String,
}

pub fn update(
    c: &Connection,
    bot: &str,
    label: &str,
    expected: Option<u64>,
) -> Result<CurrentTask> {
    ensure!(
        !crate::workspace_transfer::frozen(c)?,
        "This workspace is being moved"
    );
    let label = label.trim();
    ensure!(
        label.chars().count() <= 100 && !label.chars().any(char::is_control),
        "Use a single-line task label up to 100 characters"
    );
    let raw: String = c
        .query_row("SELECT profile FROM bots WHERE id=?", [bot], |r| r.get(0))
        .context("Bot not found")?;
    let profile: BotProfile = serde_json::from_str(&raw)?;
    ensure!(!profile.archived, "This bot is archived");
    ensure!(
        expected.is_none_or(|r| r == profile.current_task.revision),
        "The task label changed. Read its current value before editing it"
    );
    // Even clearing an already-empty label advances the revision: old in-flight
    // assignments must not resurrect a label the person explicitly removed.
    let task = CurrentTask {
        label: label.into(),
        writer_run: String::new(),
        revision: profile
            .current_task
            .revision
            .checked_add(1)
            .context("Task revision limit reached")?,
    };
    c.execute(
        "UPDATE bots SET profile=json_set(profile,'$.current_task',json(?)) WHERE id=?",
        params![serde_json::to_string(&task)?, bot],
    )?;
    Ok(task)
}

// Snapshot once per turn. Later reads cannot grant an older turn permission
// to overwrite a label changed by a person or a different turn meanwhile.
pub fn snapshot(c: &Connection, run: &str) -> Result<()> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND kind='task_label_snapshot')",
        [run],
        |r| r.get(0),
    )?;
    if !exists {
        let values: Vec<(String, u64)> = c
            .prepare(
                "SELECT id,COALESCE(json_extract(profile,'$.current_task.revision'),0) FROM bots",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let values: serde_json::Map<String, serde_json::Value> = values
            .into_iter()
            .map(|(id, r)| (id, serde_json::json!(r)))
            .collect();
        c.execute(
            "INSERT INTO events(run_id,kind,body,created) VALUES(?,'task_label_snapshot',?,?)",
            params![run, serde_json::to_string(&values)?, db::now()],
        )?;
    }
    Ok(())
}
pub fn update_by_bot(
    c: &Connection,
    run: &str,
    bot: &str,
    label: &str,
    expected: u64,
    expected_label: &str,
) -> Result<CurrentTask> {
    let raw: String = c.query_row("SELECT profile FROM bots WHERE id=?", [bot], |r| r.get(0))?;
    let current: BotProfile = serde_json::from_str(&raw)?;
    let saved:Option<String>=c.query_row("SELECT body FROM events WHERE run_id=? AND kind='task_label_snapshot' ORDER BY seq LIMIT 1",[run],|r|r.get(0)).optional()?;
    let saved: serde_json::Value = serde_json::from_str(
        &saved.context("Task label snapshot is unavailable; leave the label unchanged")?,
    )?;
    ensure!(
        current.current_task.writer_run == run
            || saved[bot].as_u64() == Some(current.current_task.revision),
        "This task label changed after your turn began. Leave the newer assignment unchanged"
    );
    ensure!(
        current.current_task.label == expected_label,
        "The task label changed; do not clear or replace a different assignment"
    );
    let mut task = update(c, bot, label, Some(expected))?;
    task.writer_run = run.into();
    c.execute(
        "UPDATE bots SET profile=json_set(profile,'$.current_task',json(?)) WHERE id=?",
        params![serde_json::to_string(&task)?, bot],
    )?;
    Ok(task)
}

pub fn is_slash(prompt: &str) -> bool {
    matches!(
        prompt.split_whitespace().next(),
        Some("/task" | "/task-remove")
    )
}

pub fn slash(c: &Connection, chat: &Chat, prompt: &str, mentions: &[String]) -> Result<bool> {
    let (command, rest) = prompt
        .trim()
        .split_once(char::is_whitespace)
        .unwrap_or((prompt.trim(), ""));
    if !matches!(command, "/task" | "/task-remove") {
        return Ok(false);
    }
    ensure!(!chat.archived, "This chat is archived");
    let mut rest = rest.trim();
    let mut named = Vec::new();
    // Longest name wins (e.g. @Alex Smith before @Alex). Only a leading
    // recipient is removed; mentions within the label remain literal text.
    let mut members = Vec::new();
    for id in &chat.members {
        if let Some(name) = c
            .query_row("SELECT name FROM bots WHERE id=?", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            members.push((id.clone(), name));
        }
    }
    members.sort_by_key(|(_, name)| std::cmp::Reverse(name.len()));
    for (id, name) in &members {
        if let Some(tail) = rest.strip_prefix(&format!("@{name}")) {
            if tail.is_empty() || tail.starts_with(char::is_whitespace) {
                ensure!(
                    members.iter().filter(|(_, n)| n == name).count() == 1,
                    "Two bots share this name; give them distinct names before assigning in a group"
                );
                named.push(id.clone());
                rest = tail.trim();
                break;
            }
        }
    }
    let target = if chat.id.starts_with("dm-") {
        ensure!(
            named.is_empty() || named[0] == chat.members[0],
            "Use this bot’s chat to change its task"
        );
        chat.members.first().context("Bot not found")?
    } else {
        ensure!(
            named.len() == 1,
            "In a group, use /task @Bot label or /task-remove @Bot"
        );
        ensure!(
            mentions.iter().all(|id| id == &named[0]),
            "Choose just one bot for this task label"
        );
        &named[0]
    };
    if command == "/task" {
        ensure!(!rest.is_empty(), "Add a task label after /task");
    } else {
        ensure!(rest.is_empty(), "Use /task-remove without a label");
    }
    update(c, target, rest, None)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::Db, tests};
    #[test]
    fn stale_cleanup_and_preferences_cannot_erase_new_assignment() {
        let db = Db::open(":memory:").unwrap();
        let mut bot = tests::bot(&db, "codex");
        let first = update(&db.0.lock().unwrap(), &bot.id, "First", Some(0)).unwrap();
        let newer = update(&db.0.lock().unwrap(), &bot.id, "Next", Some(first.revision)).unwrap();
        assert!(update(&db.0.lock().unwrap(), &bot.id, "", Some(first.revision)).is_err());
        bot.profile.color = "#112233".into();
        db.save_bot_preferences(&bot, true).unwrap();
        assert_eq!(db.bot(&bot.id).unwrap().profile.current_task.label, "Next");
        update(&db.0.lock().unwrap(), &bot.id, "", Some(newer.revision)).unwrap();
        assert!(
            update(
                &db.0.lock().unwrap(),
                &bot.id,
                "First",
                Some(newer.revision)
            )
            .is_err()
        );
    }
    #[test]
    fn slash_labels_do_not_queue_work_and_retry_is_idempotent() {
        let db = Db::open(":memory:").unwrap();
        let bot = tests::bot(&db, "codex");
        let chat = format!("dm-{}", bot.id);
        db.0.lock()
            .unwrap()
            .execute(
                "INSERT INTO chats(id,name,members) VALUES(?,?,?)",
                params![chat, bot.name, serde_json::json!([bot.id]).to_string()],
            )
            .unwrap();
        let key = db::id();
        for _ in 0..2 {
            assert!(
                db.chat_send_request(&chat, "/task ACME External Pen", &[], &[], None, Some(&key))
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(db.bot(&bot.id).unwrap().profile.current_task.revision, 1);
        assert_eq!(db.chat_messages(&chat).unwrap().len(), 0);
        assert!(db.runs(None).unwrap().is_empty());
        db.chat_send(&chat, "/task-remove", &[]).unwrap();
        assert!(
            db.bot(&bot.id)
                .unwrap()
                .profile
                .current_task
                .label
                .is_empty()
        );
        assert!(db.chat_send(&chat, "/task", &[]).is_err());
        assert!(db.chat_send(&chat, "/task-remove unwanted", &[]).is_err());
    }
    #[test]
    fn older_turn_cannot_clear_or_resurrect_newer_label_even_with_latest_revision() {
        let db = Db::open(":memory:").unwrap();
        let bot = tests::bot(&db, "codex");
        update(&db.0.lock().unwrap(), &bot.id, "First", Some(0)).unwrap();
        let run = db.queue(&bot.id, "Work", 0).unwrap();
        snapshot(&db.0.lock().unwrap(), &run).unwrap();
        let next = update(&db.0.lock().unwrap(), &bot.id, "Next", Some(1)).unwrap();
        assert!(
            update_by_bot(
                &db.0.lock().unwrap(),
                &run,
                &bot.id,
                "",
                next.revision,
                "Next"
            )
            .is_err()
        );
        let clear = update(&db.0.lock().unwrap(), &bot.id, "", Some(next.revision)).unwrap();
        assert!(
            update_by_bot(
                &db.0.lock().unwrap(),
                &run,
                &bot.id,
                "First",
                clear.revision,
                ""
            )
            .is_err()
        );
        let later = db.queue(&bot.id, "New work", 0).unwrap();
        snapshot(&db.0.lock().unwrap(), &later).unwrap();
        let owned = update_by_bot(
            &db.0.lock().unwrap(),
            &later,
            &bot.id,
            "New work",
            clear.revision,
            "",
        )
        .unwrap();
        update_by_bot(
            &db.0.lock().unwrap(),
            &later,
            &bot.id,
            "",
            owned.revision,
            "New work",
        )
        .unwrap();
    }
    #[test]
    fn group_requires_one_explicit_member_and_invalid_files_roll_back() {
        let db = Db::open(":memory:").unwrap();
        let mut a = tests::bot(&db, "codex");
        a.name = "Piper".into();
        db.save_bot(&a).unwrap();
        let mut b = tests::bot(&db, "codex");
        b.name = "Rowan".into();
        db.save_bot(&b).unwrap();
        db.0.lock()
            .unwrap()
            .execute(
                "INSERT INTO chats(id,name,members) VALUES('group','Team',?)",
                [serde_json::json!([a.id, b.id]).to_string()],
            )
            .unwrap();
        assert!(db.chat_send("group", "/task Work", &[]).is_err());
        assert!(db.chat_send("group", "/task @Unknown Work", &[]).is_err());
        db.chat_send("group", "/task @Rowan ACME External Pen", &[b.id.clone()])
            .unwrap();
        assert_eq!(
            db.bot(&b.id).unwrap().profile.current_task.label,
            "ACME External Pen"
        );
        assert!(db.bot(&a.id).unwrap().profile.current_task.label.is_empty());
        assert!(
            db.chat_send_files(
                "group",
                "/task @Rowan Other",
                &[b.id.clone()],
                &["missing".into()]
            )
            .is_err()
        );
        assert_eq!(
            db.bot(&b.id).unwrap().profile.current_task.label,
            "ACME External Pen"
        );
        db.chat_send("group", "/task-remove @Rowan", &[b.id.clone()])
            .unwrap();
        assert!(db.bot(&b.id).unwrap().profile.current_task.label.is_empty());
        assert!(db.runs(None).unwrap().is_empty());
    }
    #[tokio::test]
    async fn bot_tool_can_label_teammate_without_delegating_or_starting_work() {
        let app = tests::app();
        let actor = tests::bot(&app.db, "codex");
        let target = tests::bot(&app.db, "codex");
        let id = app.db.queue(&actor.id, "Assign the label", 0).unwrap();
        let run = app.db.run(&id).unwrap();
        snapshot(&app.db.0.lock().unwrap(), &id).unwrap();
        let result=crate::runtime::call_tool(&app,&actor,&run,"bot_task_update",serde_json::json!({"bot_id":target.id,"label":"ACME External Pen","expected_revision":0,"expected_label":""})).await.unwrap();
        assert_ne!(result["failed"], true);
        assert_eq!(
            app.db.bot(&target.id).unwrap().profile.current_task.label,
            "ACME External Pen"
        );
        assert_eq!(app.db.runs(None).unwrap().len(), 1);
        assert!(app.db.run_approvals(&id).unwrap().is_empty());
    }
    #[test]
    fn archive_clears_label_and_run_endpoint_rejects_label_commands() {
        let db = Db::open(":memory:").unwrap();
        let mut bot = tests::bot(&db, "codex");
        update(&db.0.lock().unwrap(), &bot.id, "Old assignment", Some(0)).unwrap();
        assert!(db.queue(&bot.id, "/task New assignment", 0).is_err());
        assert_eq!(
            db.bot(&bot.id).unwrap().profile.current_task.label,
            "Old assignment"
        );
        bot.profile.archived = true;
        db.save_bot_preferences(&bot, true).unwrap();
        bot.profile.archived = false;
        db.save_bot_preferences(&bot, true).unwrap();
        assert!(
            db.bot(&bot.id)
                .unwrap()
                .profile
                .current_task
                .label
                .is_empty()
        );
        assert!(db.runs(None).unwrap().is_empty());
    }
    #[test]
    fn older_custom_commands_get_unique_names_without_losing_content() {
        let db = Db::open(":memory:").unwrap();
        let c = db.0.lock().unwrap();
        c.execute(
            "INSERT INTO skills(name,body,command) VALUES('Old task','Keep this workflow','task')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO skills(name,body,command) VALUES('Existing alias','Keep too','skill-task')",[]).unwrap();
        crate::commands::migrate(&c).unwrap();
        let value: (String, String) = c
            .query_row(
                "SELECT command,body FROM skills WHERE name='Old task'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(value, ("skill-task-1".into(), "Keep this workflow".into()));
        crate::commands::migrate(&c).unwrap();
        assert_eq!(
            c.query_row(
                "SELECT command FROM skills WHERE name='Old task'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "skill-task-1"
        );
    }
    #[test]
    fn labels_survive_reopen_and_invalid_updates_are_atomic() {
        let root = std::env::temp_dir().join(format!("kindred-task-{}", db::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("db.sqlite");
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = tests::bot(&db, "codex");
        update(&db.0.lock().unwrap(), &bot.id, "Persistent", Some(0)).unwrap();
        assert!(update(&db.0.lock().unwrap(), &bot.id, "bad\nlabel", Some(1)).is_err());
        assert!(update(&db.0.lock().unwrap(), &bot.id, &"x".repeat(101), Some(1)).is_err());
        drop(db);
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            db.bot(&bot.id).unwrap().profile.current_task.label,
            "Persistent"
        );
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}
