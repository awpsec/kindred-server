use crate::db::Db;
use anyhow::{Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

impl Db {
    pub fn attention(&self) -> Result<Value> {
        let mutes = self.setting("notification_mutes")?.unwrap_or(json!({}));
        let c = self.0.lock().unwrap();
        let rows = c.prepare("SELECT c.id,COALESCE((SELECT MAX(m.seq) FROM chat_messages m WHERE m.chat_id=c.id AND m.suppressed=0 AND m.kind!='connector_artifact' AND m.sender NOT IN ('user','system') AND trim(m.body)!=''),0),COALESCE(r.message_seq,0),COALESCE((SELECT m.seq FROM chat_messages m WHERE m.chat_id=c.id AND m.suppressed=0 AND m.kind!='connector_artifact' AND trim(m.body)!='' ORDER BY m.created DESC,COALESCE(m.history_order,printf('%020d:%020d',m.seq,0)) DESC LIMIT 1),0) FROM chats c LEFT JOIN chat_reads r ON r.chat_id=c.id")?
            .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut chats: serde_json::Map<String, Value> = rows.into_iter().map(|(id,cursor,read,last)|
            (id,json!({"cursor":cursor,"read_cursor":read,"latest_message_seq":last,"unread":cursor>read}))).collect();
        for (id, value) in &mut chats {
            if value["unread"] == true {
                let read = value["read_cursor"].as_i64().unwrap_or(0);
                let first: Option<i64> = c.query_row("SELECT (SELECT seq FROM chat_messages WHERE chat_id=? AND seq>? AND suppressed=0 AND kind!='connector_artifact' AND sender NOT IN ('user','system') AND trim(body)!='' ORDER BY created,COALESCE(history_order,printf('%020d:%020d',seq,0)) LIMIT 1)", params![id,read], |r| r.get(0))?;
                value["first_unread_seq"] = json!(first);
            }
        }
        // Query persisted history, including bots outside the recent-runs feed.
        let rows = c.prepare("SELECT b.id,MAX(COALESCE((SELECT MAX(r.created) FROM runs r WHERE r.bot_id=b.id),0),COALESCE((SELECT MAX(e.created) FROM events e JOIN runs r ON r.id=e.run_id WHERE r.bot_id=b.id),0),COALESCE((SELECT MAX(m.created) FROM chat_messages m WHERE m.sender=b.id),0)) FROM bots b")?
            .query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let bots: serde_json::Map<String, Value> = rows
            .into_iter()
            .map(|(id, time)| (id, json!({"last_active_at":time})))
            .collect();
        Ok(json!({"chats":chats,"bots":bots,"mutes":mutes}))
    }

    pub fn mark_chat_read(&self, id: &str, cursor: i64) -> Result<()> {
        ensure!(cursor >= 0, "Invalid read cursor");
        let c = self.0.lock().unwrap();
        let exists: bool =
            c.query_row("SELECT EXISTS(SELECT 1 FROM chats WHERE id=?)", [id], |r| {
                r.get(0)
            })?;
        ensure!(exists, "Chat not found");
        let latest: i64 = c.query_row("SELECT COALESCE(MAX(seq),0) FROM chat_messages WHERE chat_id=? AND suppressed=0 AND sender NOT IN ('user','system') AND trim(body)!=''", [id], |r| r.get(0))?;
        ensure!(cursor <= latest, "Read cursor is ahead of this chat");
        c.execute("INSERT INTO chat_reads(chat_id,message_seq) VALUES(?,?) ON CONFLICT(chat_id) DO UPDATE SET message_seq=MAX(message_seq,excluded.message_seq)", params![id,cursor])?;
        Ok(())
    }

    pub fn save_bot_identity(
        &self,
        id: &str,
        name: &str,
        label: &str,
        description: &str,
        notifications: bool,
    ) -> Result<()> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 80,
            "Name must be 1..80 bytes"
        );
        ensure!(
            label.len() <= 80 && description.len() <= 2000,
            "Label or description is too long"
        );
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let changed = tx.execute("UPDATE bots SET name=?1,profile=json_set(profile,'$.label',?2,'$.description',?3,'$.notifications',json(?4)) WHERE id=?5", params![name.trim(),label,description,if notifications {"true"} else {"false"},id])?;
        ensure!(changed == 1, "Bot not found");
        tx.execute(
            "UPDATE chats SET name=? WHERE id=?",
            params![name.trim(), format!("dm-{id}")],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{app, bot};

    #[test]
    fn attention_receipts_persist_and_stale_reads_leave_new_replies_unread() {
        let root = std::env::temp_dir().join(format!("kindred-attention-{}", crate::db::id()));
        let path = root.join("kindred.db").to_string_lossy().into_owned();
        let db = Db::open(&path).unwrap();
        let b = bot(&db, "codex");
        let run = db.queue(&b.id, "Hello", 0).unwrap();
        let chat = format!("dm-{}", b.id);
        db.event(&run, "reasoning", json!({"text":"Private"}))
            .unwrap();
        assert_eq!(db.attention().unwrap()["chats"][&chat]["unread"], false);
        db.event(&run, "assistant", json!({"text":"First reply"}))
            .unwrap();
        let first = db.attention().unwrap()["chats"][&chat]["cursor"]
            .as_i64()
            .unwrap();
        db.event(&run, "assistant", json!({"text":"Second reply"}))
            .unwrap();
        db.mark_chat_read(&chat, first).unwrap();
        let attention = db.attention().unwrap();
        assert_eq!(attention["chats"][&chat]["unread"], true);
        let second = attention["chats"][&chat]["cursor"].as_i64().unwrap();
        assert_eq!(attention["chats"][&chat]["first_unread_seq"], second);
        assert!(db.mark_chat_read(&chat, second + 1).is_err());
        assert!(db.mark_chat_read("missing", 0).is_err());
        assert!(db.mark_chat_read(&chat, -1).is_err());
        db.mark_chat_read(&chat, second).unwrap();
        db.mark_chat_read(&chat, first).unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.attention().unwrap()["chats"][&chat]["unread"], false);
        assert_eq!(
            db.attention().unwrap()["chats"][&chat]["read_cursor"],
            second
        );
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn connector_only_history_does_not_create_unread_but_real_replies_do() {
        let app = app();
        let b = bot(&app.db, "codex");
        let run = app.db.queue(&b.id, "Check inbox", 0).unwrap();
        let chat = format!("dm-{}", b.id);
        let insert_call = || {
            app.db.0.lock().unwrap().execute(
                "INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'connector_artifact',?,?)",
                params![chat,b.id,"gmail-search",&run,crate::db::now()],
            ).unwrap();
        };
        for _ in 0..5 {
            insert_call();
        }
        let quiet = app.db.attention().unwrap();
        assert_eq!(quiet["chats"][&chat]["unread"], false);
        assert_eq!(quiet["chats"][&chat]["cursor"], 0);
        app.db
            .event(&run, "assistant", json!({"text":"A bill arrived."}))
            .unwrap();
        let reply = app.db.attention().unwrap()["chats"][&chat]["cursor"]
            .as_i64()
            .unwrap();
        insert_call();
        let unread = app.db.attention().unwrap();
        assert_eq!(unread["chats"][&chat]["cursor"], reply);
        assert_eq!(unread["chats"][&chat]["first_unread_seq"], reply);
        assert_eq!(unread["chats"][&chat]["latest_message_seq"], reply);
        app.db.mark_chat_read(&chat, reply).unwrap();
        insert_call();
        assert_eq!(app.db.attention().unwrap()["chats"][&chat]["unread"], false);
        // Receipts issued by an older client may point at an activity record.
        let old_cursor: i64 = app
            .db
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT MAX(seq) FROM chat_messages WHERE chat_id=?",
                [&chat],
                |r| r.get(0),
            )
            .unwrap();
        app.db.mark_chat_read(&chat, old_cursor).unwrap();
        app.db
            .event(&run, "assistant", json!({"text":"Another bill arrived."}))
            .unwrap();
        assert_eq!(app.db.attention().unwrap()["chats"][&chat]["unread"], true);
    }

    #[test]
    fn attention_activity_uses_persisted_history_and_identity_preserves_configuration() {
        let app = app();
        let mut b = bot(&app.db, "codex");
        b.profile = serde_json::from_value(
            json!({"shape":"triangle","color":"#123456","pinned":true,"archived":false}),
        )
        .unwrap();
        b.memory = "Keep this".into();
        app.db.save_bot(&b).unwrap();
        assert_eq!(
            app.db.attention().unwrap()["bots"][&b.id]["last_active_at"],
            0
        );
        let old = app.db.queue(&b.id, "Old task", 0).unwrap();
        app.db
            .event(&old, "tool_requested", json!({"tool":"fixture"}))
            .unwrap();
        {
            let c = app.db.0.lock().unwrap();
            c.execute("UPDATE runs SET created=100 WHERE id=?", [&old])
                .unwrap();
            c.execute("UPDATE events SET created=200 WHERE run_id=?", [&old])
                .unwrap();
        }
        let other = bot(&app.db, "codex");
        for _ in 0..105 {
            let id = app.db.queue(&other.id, "Recent", 0).unwrap();
            app.db.finish(&id, "completed", "Done", "").unwrap();
        }
        assert_eq!(
            app.db.attention().unwrap()["bots"][&b.id]["last_active_at"],
            200
        );
        app.db
            .save_bot_identity(&b.id, "  New name  ", "Inbox", "Description", false)
            .unwrap();
        let saved = app.db.bot(&b.id).unwrap();
        assert_eq!(saved.name, "New name");
        assert_eq!(saved.memory, b.memory);
        assert_eq!(saved.instructions, b.instructions);
        assert_eq!(saved.provider, b.provider);
        assert_eq!(saved.model, b.model);
        for key in ["shape", "color", "pinned", "archived"] {
            assert_eq!(
                serde_json::to_value(&saved.profile).unwrap()[key],
                serde_json::to_value(&b.profile).unwrap()[key]
            );
        }
        assert_eq!(saved.profile.notifications, false);
        assert_eq!(saved.profile.label, "Inbox");
        assert!(app.db.save_bot_identity(&b.id, " ", "", "", true).is_err());
        assert_eq!(app.db.bot(&b.id).unwrap().name, "New name");
    }

    #[tokio::test]
    async fn attention_and_identity_routes_require_authentication() {
        let app = app();
        let b = bot(&app.db, "codex");
        let run = app.db.queue(&b.id, "Hello", 0).unwrap();
        app.db
            .event(&run, "assistant", json!({"text":"Hello back"}))
            .unwrap();
        let chat = format!("dm-{}", b.id);
        let cursor = app.db.attention().unwrap()["chats"][&chat]["cursor"].clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{base}/attention"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        for (path, body) in [
            (format!("chats/{chat}/read"), json!({"cursor":cursor})),
            (
                format!("bots/{}/identity", b.id),
                json!({"name":"New","label":"Role","description":"Description","notifications":false}),
            ),
        ] {
            let url = format!("{base}/{path}");
            assert_eq!(
                client.put(&url).json(&body).send().await.unwrap().status(),
                401
            );
            assert!(
                client
                    .put(&url)
                    .bearer_auth(&app.token)
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .is_success()
            );
        }
        assert_eq!(app.db.attention().unwrap()["chats"][&chat]["unread"], false);
        assert_eq!(app.db.bot(&b.id).unwrap().name, "New");
        server.abort();
    }
}
