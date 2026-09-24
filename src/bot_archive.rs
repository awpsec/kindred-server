//! Owner-only permanent removal; archiving itself remains reversible.
use crate::db::Db;
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
impl Db {
    pub fn delete_archived_bot(&self, id: &str, expected_name: &str) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        ensure!(
            !crate::workspace_transfer::frozen(&tx)?,
            "This workspace is paused"
        );
        let (name, archived): (String, bool) = tx
            .query_row(
                "SELECT name,COALESCE(json_extract(profile,'$.archived'),0) FROM bots WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("This bot no longer exists"))?;
        ensure!(archived, "Archive the bot before permanently deleting it");
        ensure!(
            name == expected_name,
            "The bot's name changed. Review the deletion again"
        );
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE bot_id=?1 AND status IN ('queued','running','awaiting_user','awaiting_approval','cancelling')) OR EXISTS(SELECT 1 FROM command_jobs WHERE bot_id=?1 AND status IN ('starting','running')) OR EXISTS(SELECT 1 FROM routines WHERE bot_id=?1 AND enabled=1)",[id],|r|r.get::<_,bool>(0))?, "Stop this bot's tasks and commands and pause its routines before deleting it");
        ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM collaboration_requests c JOIN runs r ON r.id=c.child_run_id OR r.id=c.parent_run_id WHERE r.bot_id=? AND c.resolved=0)",[id],|r|r.get::<_,bool>(0))?, "Finish this bot's pending collaborations before deleting it");
        let dm = format!("dm-{id}");
        // Retain public prose, but remove links to private runs that are going away.
        tx.execute("UPDATE chat_messages SET body='This item was removed when its bot was permanently deleted.',kind='message' WHERE chat_id<>?1 AND run_id IN (SELECT id FROM runs WHERE bot_id=?2) AND kind NOT IN ('message','assistant','result','handoff','notice','collaboration')",params![dm,id])?;
        tx.execute("UPDATE chat_messages SET run_id='' WHERE chat_id<>?1 AND run_id IN (SELECT id FROM runs WHERE bot_id=?2)",params![dm,id])?;
        tx.execute("UPDATE chats SET members=(SELECT json_group_array(value) FROM json_each(chats.members) WHERE value<>?1) WHERE EXISTS(SELECT 1 FROM json_each(chats.members) WHERE value=?1)",[id])?;
        tx.execute("UPDATE chats SET archived=1 WHERE json_array_length(members)=0 AND id NOT LIKE 'server-%'",[])?;
        crate::chats::archive_inactive_bot_chats(&tx)?;
        // Discover actual FK dependencies rather than disabling integrity checks or
        // maintaining a brittle list that misses newly introduced artifact tables.
        tx.execute_batch("CREATE TEMP TABLE bot_delete_rows(table_name TEXT NOT NULL,row_id INTEGER NOT NULL,PRIMARY KEY(table_name,row_id)); PRAGMA defer_foreign_keys=ON;")?;
        // Virtual search indexes and their shadow tables are maintained by triggers.
        let tables: Vec<String> = tx.prepare("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name NOT LIKE 'sqlite_%'")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut edges = vec![];
        for table in &tables {
            if table == "provider_usage" {
                continue;
            } // Keep billing/usage audit history.
            let columns: Vec<String> = tx
                .prepare(&format!("PRAGMA table_info({})", quoted(table)))?
                .query_map([], |r| r.get(1))?
                .collect::<rusqlite::Result<_>>()?;
            let mut clauses = vec![];
            if table == "bots" {
                clauses.push("id=?1".to_owned());
            }
            if table == "chats" {
                clauses.push("id=?2".to_owned());
            }
            if columns.iter().any(|c| c == "bot_id") {
                clauses.push("bot_id=?1".to_owned());
            }
            if columns.iter().any(|c| c == "chat_id") {
                clauses.push("chat_id=?2".to_owned());
            }
            if table != "chat_messages" && columns.iter().any(|c| c == "run_id") {
                clauses.push("run_id IN (SELECT id FROM runs WHERE bot_id=?1)".to_owned());
            }
            if !clauses.is_empty() {
                tx.execute(
                    &format!(
                        "INSERT OR IGNORE INTO bot_delete_rows SELECT ?3,rowid FROM {} WHERE {}",
                        quoted(table),
                        clauses.join(" OR ")
                    ),
                    params![id, dm, table],
                )?;
            }
            let fks: Vec<(String, String, String)> = tx
                .prepare(&format!("PRAGMA foreign_key_list({})", quoted(table)))?
                .query_map([], |r| Ok((r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (parent, from, to) in fks {
                edges.push((table.clone(), parent, from, to));
            }
        }
        loop {
            let mut added = 0;
            for (child, parent, from, to) in &edges {
                added+=tx.execute(&format!("INSERT OR IGNORE INTO bot_delete_rows SELECT ?1,rowid FROM {} WHERE {} IN (SELECT {} FROM {} WHERE rowid IN (SELECT row_id FROM bot_delete_rows WHERE table_name=?2))",quoted(child),quoted(from),quoted(to),quoted(parent)),params![child,parent])?;
            }
            if added == 0 {
                break;
            }
        }
        for table in &tables {
            if table == "provider_usage" {
                continue;
            }
            tx.execute(&format!("DELETE FROM {} WHERE rowid IN (SELECT row_id FROM bot_delete_rows WHERE table_name=?)",quoted(table)),[table])?;
        }
        tx.execute_batch("DROP TABLE bot_delete_rows;")?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delete_requires_archive_and_exact_identity_and_preserves_other_bot() {
        let app = crate::tests::app();
        let mut b = crate::tests::bot(&app.db, "codex");
        let peer = crate::tests::bot(&app.db, "codex");
        assert!(app.db.delete_archived_bot(&b.id, &b.name).is_err());
        let run = app.db.queue(&b.id, "private task", 0).unwrap();
        app.db
            .finish(&run, "completed", "private result", "")
            .unwrap();
        app.db
            .event(
                &run,
                "assistant",
                serde_json::json!({"text":"private result"}),
            )
            .unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO attachments VALUES('file',?,'test',?,1)",
                params![run, vec![1u8; 1024]],
            )
            .unwrap();
        b.profile.archived = true;
        app.db.save_bot(&b).unwrap();
        assert!(app.db.delete_archived_bot(&b.id, "wrong name").is_err());
        app.db.delete_archived_bot(&b.id, &b.name).unwrap();
        assert!(app.db.bot(&b.id).is_err());
        assert!(app.db.bot(&peer.id).is_ok());
        assert!(app.db.run(&run).is_err());
        let c = app.db.0.lock().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM attachments", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(
            !c.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap()
        );
    }
    #[test]
    fn active_work_blocks_deletion_without_partial_changes() {
        let app = crate::tests::app();
        let b = crate::tests::bot(&app.db, "codex");
        let run = app.db.queue(&b.id, "still working", 0).unwrap();
        // Simulate an archived record with work still present (legacy state/race).
        app.db.0.lock().unwrap().execute(
            "UPDATE bots SET profile=json_set(profile,'$.archived',json('true')) WHERE id=?",
            [&b.id],
        ).unwrap();
        assert!(app.db.delete_archived_bot(&b.id, &b.name).is_err());
        assert!(app.db.bot(&b.id).is_ok());
        assert!(app.db.run(&run).is_ok());
        app.db.finish(&run, "completed", "done", "").unwrap();
        app.db.delete_archived_bot(&b.id, &b.name).unwrap();
    }
    #[test]
    fn deleting_archived_bot_preserves_shared_prose_and_other_members() {
        let app = crate::tests::app();
        let mut b = crate::tests::bot(&app.db, "codex");
        let peer = crate::tests::bot(&app.db, "codex");
        let chat = crate::chats::Chat {
            id: "group-delete-test".into(),
            name: "Team".into(),
            description: String::new(),
            bot_only: false,
            members: vec![b.id.clone(), peer.id.clone()],
            archived: false,
            pinned: false,
            last_message: None,
        };
        app.db.save_chat(&chat).unwrap();
        app.db.0.lock().unwrap().execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,?,?,'message',1)",params![chat.id,b.id,"Shared finding"]).unwrap();
        b.profile.archived = true;
        app.db.save_bot(&b).unwrap();
        app.db.delete_archived_bot(&b.id, &b.name).unwrap();
        assert_eq!(app.db.chat(&chat.id).unwrap().members, vec![peer.id]);
        assert_eq!(
            app.db.chat_messages(&chat.id).unwrap()[0]["text"],
            "Shared finding"
        );
    }
}
