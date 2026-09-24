//! User message actions retain exact conversation and message identity.
use crate::db::{Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub const EMOJI: &[&str] = &[
    "👍", "❤️", "😂", "🎉", "👀", "🙏", "✅", "🤔", "😮", "😢", "🔥", "💯", "👎", "🙌", "✨", "💡",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reaction {
    pub emoji: Option<String>,
}

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS user_message_reactions(message_seq INTEGER PRIMARY KEY REFERENCES chat_messages(seq),emoji TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS message_replies(message_seq INTEGER PRIMARY KEY REFERENCES chat_messages(seq),reply_to_seq INTEGER NOT NULL REFERENCES chat_messages(seq));
      CREATE TABLE IF NOT EXISTS run_message_sources(run_id TEXT PRIMARY KEY REFERENCES runs(id),message_seq INTEGER NOT NULL REFERENCES chat_messages(seq));")?;
    Ok(())
}

pub fn quoted_message(c: &Connection, chat: &str, seq: i64) -> Result<Value> {
    let (sender, text, kind): (String, String, String) = c
        .query_row(
            "SELECT sender,body,kind FROM chat_messages WHERE seq=? AND chat_id=?",
            params![seq, chat],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("Message does not belong to this conversation"))?;
    ensure!(
        [
            "message",
            // Old generated continuation messages may already have replies.
            "continuation",
            "assistant",
            "result",
            "handoff",
            "question",
            "connector_artifact"
        ]
        .contains(&kind.as_str())
            && !text.trim().is_empty(),
        "This message does not support replies or reactions"
    );
    let text = if kind == "connector_artifact" {
        let card = crate::connector_artifacts::record(c, &text)?;
        json!({"connector":card["connection"],"tool":card["tool"],"status":card["status"],"title":card["title"],"input":card["input"],"records":card["records"],"artifact_id":card["id"]}).to_string()
    } else if kind == "question" {
        c.query_row("SELECT question||char(10)||context||CASE WHEN status='answered' THEN char(10)||'Answer: '||answer ELSE '' END FROM questions WHERE id=?",[text],|r|r.get::<_,String>(0))?
    } else {
        text
    };
    let author: String = if sender == "user" {
        "You".into()
    } else {
        c.query_row("SELECT name FROM bots WHERE id=?", [&sender], |r| r.get(0))
            .optional()?
            .unwrap_or_else(|| "Kindred".into())
    };
    // A large source cannot crowd out the user's new request or model context.
    let excerpt: String = text.chars().take(8000).collect();
    Ok(
        json!({"seq":seq,"sender":sender,"author":author,"text":excerpt,"kind":kind,"truncated":text.chars().count()>8000}),
    )
}

impl Db {
    pub fn user_react(&self, chat: &str, seq: i64, emoji: Option<&str>) -> Result<()> {
        ensure!(
            emoji.is_none_or(|e| EMOJI.contains(&e)),
            "Unsupported reaction"
        );
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM chats WHERE id=? AND archived=0)",
            [chat],
            |r| r.get(0),
        )?;
        ensure!(active, "This conversation is archived or unavailable");
        quoted_message(&tx, chat, seq)?;
        if let Some(emoji) = emoji {
            tx.execute("INSERT INTO user_message_reactions VALUES(?,?) ON CONFLICT(message_seq) DO UPDATE SET emoji=excluded.emoji",params![seq,emoji])?;
        } else {
            tx.execute(
                "DELETE FROM user_message_reactions WHERE message_seq=?",
                [seq],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn decorate_message_actions(&self, row: &mut Value) -> Result<()> {
        let c = self.0.lock().unwrap();
        let seq = row["seq"].as_i64().unwrap();
        if let Some(emoji) = c
            .query_row(
                "SELECT emoji FROM user_message_reactions WHERE message_seq=?",
                [seq],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            row["reactions"]
                .as_array_mut()
                .unwrap()
                .push(json!({"user":true,"emoji":emoji}));
        }
        let target: Option<(String,i64)>=c.query_row("SELECT m.chat_id,r.reply_to_seq FROM message_replies r JOIN chat_messages m ON m.seq=r.message_seq WHERE r.message_seq=?",[seq],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((chat, target)) = target {
            row["reply_to"] = quoted_message(&c, &chat, target)?;
        }
        Ok(())
    }
    pub fn quoted_context_for_run(&self, run: &Run) -> Result<Option<Value>> {
        let c = self.0.lock().unwrap();
        let target: Option<i64>=c.query_row("SELECT p.reply_to_seq FROM run_message_sources s JOIN message_replies p ON p.message_seq=s.message_seq JOIN chat_messages m ON m.seq=s.message_seq WHERE s.run_id=? AND m.chat_id=?",params![run.id,run.chat_id],|r|r.get(0)).optional()?;
        target
            .map(|seq| quoted_message(&c, &run.chat_id, seq))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chats::Chat,
        db, runtime,
        tests::{app, bot},
        web,
    };
    fn message(db: &Db, chat: &str, sender: &str, text: &str) -> i64 {
        if db.chat(chat).is_err() {
            db.save_chat(&Chat {
                bot_only: false,
                description: String::new(),
                id: chat.into(),
                name: "Fixture DM".into(),
                members: vec![sender.into()],
                archived: false,
                pinned: false,
                last_message: None,
            })
            .unwrap();
        }
        let c = db.0.lock().unwrap();
        c.execute(
            "INSERT INTO chat_messages(chat_id,sender,body,kind,created) VALUES(?,?,?,'message',?)",
            params![chat, sender, text, db::now()],
        )
        .unwrap();
        c.last_insert_rowid()
    }
    #[test]
    fn replies_retain_exact_source_and_route_to_author_without_changing_plain_messages() {
        let app = app();
        let a = bot(&app.db, "codex");
        let b = bot(&app.db, "codex");
        let chat = Chat {
            bot_only: false,
            description: String::new(),
            id: db::id(),
            name: "Reply check".into(),
            members: vec![a.id.clone(), b.id.clone()],
            archived: false,
            pinned: false,
            last_message: None,
        };
        app.db.save_chat(&chat).unwrap();
        let source = message(
            &app.db,
            &chat.id,
            &b.id,
            "The parcel reference is MAPLE-4729.",
        );
        // Put the quote outside recent context and the first history page.
        for _ in 0..65 {
            message(&app.db, &chat.id, &a.id, "An unrelated update.");
        }
        let run_id = app
            .db
            .chat_send_message(&chat.id, "What is the reference?", &[], &[], Some(source))
            .unwrap()
            .remove(0);
        let run = app.db.run(&run_id).unwrap();
        assert_eq!(run.bot_id, b.id);
        assert_eq!(run.prompt, "What is the reference?");
        assert_eq!(
            app.db.quoted_context_for_run(&run).unwrap().unwrap()["text"],
            "The parcel reference is MAPLE-4729."
        );
        let instruction = runtime::instructions(&app, &b, &run).unwrap();
        let context: Value =
            serde_json::from_str(instruction.rsplit_once('\n').unwrap().1).unwrap();
        assert_eq!(context["selected_reply"]["seq"], source);
        assert_eq!(
            context["selected_reply"]["text"],
            "The parcel reference is MAPLE-4729."
        );
        let posted = app.db.chat_messages(&chat.id).unwrap().pop().unwrap();
        assert_eq!(posted["text"], "What is the reference?");
        assert_eq!(posted["reply_to"]["seq"], source);
        let explicit = app
            .db
            .chat_send_message(&chat.id, "Explain it", &[a.id.clone()], &[], Some(source))
            .unwrap();
        assert_eq!(app.db.run(&explicit[0]).unwrap().bot_id, a.id);
        let normal = app.db.chat_send(&chat.id, "New question", &[]).unwrap();
        assert_eq!(app.db.run(&normal[0]).unwrap().bot_id, a.id);
        assert!(
            app.db
                .quoted_context_for_run(&app.db.run(&normal[0]).unwrap())
                .unwrap()
                .is_none()
        );
        let own = message(&app.db, &chat.id, "user", "My earlier thought");
        let self_reply = app
            .db
            .chat_send_message(&chat.id, "Add detail", &[], &[], Some(own))
            .unwrap();
        assert_eq!(app.db.run(&self_reply[0]).unwrap().bot_id, a.id);
        let dm = format!("dm-{}", a.id);
        let other = message(&app.db, &dm, &a.id, "Private to another chat");
        let count = app.db.runs(None).unwrap().len();
        assert!(
            app.db
                .chat_send_message(&chat.id, "No", &[], &[], Some(other))
                .is_err()
        );
        assert!(
            app.db
                .chat_send_message(&chat.id, "No", &[], &["missing-file".into()], Some(source))
                .is_err()
        );
        assert_eq!(app.db.runs(None).unwrap().len(), count);
        let mut removed = chat.clone();
        removed.members = vec![a.id.clone()];
        for run in app.db.runs(None).unwrap() {
            app.db.finish(&run.id, "completed", "", "").unwrap();
        }
        app.db.save_chat(&removed).unwrap();
        assert!(
            app.db
                .chat_send_message(&chat.id, "No", &[], &[], Some(source))
                .is_err()
        );
        removed.archived = true;
        app.db.save_chat(&removed).unwrap();
        assert!(
            app.db
                .chat_send_message(&chat.id, "No", &[], &[], Some(source))
                .is_err()
        );
    }
    #[test]
    fn user_reactions_coexist_with_bot_reactions_and_survive_restart_with_reply_links() {
        let file = std::env::temp_dir().join(format!("kindred-message-actions-{}.db", db::id()));
        let path = file.to_str().unwrap();
        let db = Db::open(path).unwrap();
        let b = bot(&db, "codex");
        let chat = format!("dm-{}", b.id);
        let seq = message(&db, &chat, &b.id, "A durable source");
        db.react(&chat, &b.id, seq, "👍").unwrap();
        db.user_react(&chat, seq, Some("❤️")).unwrap();
        db.user_react(&chat, seq, Some("❤️")).unwrap();
        let run_id = db
            .chat_send_message(&chat, "Reply", &[], &[], Some(seq))
            .unwrap()
            .remove(0);
        drop(db);
        let db = Db::open(path).unwrap();
        let rows = db.chat_messages(&chat).unwrap();
        assert_eq!(rows[0]["reactions"].as_array().unwrap().len(), 2);
        assert_eq!(rows[1]["reply_to"]["seq"], seq);
        assert_eq!(
            db.quoted_context_for_run(&db.run(&run_id).unwrap())
                .unwrap()
                .unwrap()["text"],
            "A durable source"
        );
        db.user_react(&chat, seq, Some("😂")).unwrap();
        let changed = db.chat_messages(&chat).unwrap();
        assert!(
            changed[0]["reactions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["user"] == true && r["emoji"] == "😂")
        );
        db.user_react(&chat, seq, None).unwrap();
        db.user_react(&chat, seq, None).unwrap();
        assert_eq!(
            db.chat_messages(&chat).unwrap()[0]["reactions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(db.user_react(&chat, seq, Some("not an emoji")).is_err());
        assert!(db.user_react("another-chat", seq, Some("👍")).is_err());
        drop(db);
        let _ = std::fs::remove_file(file);
    }
    #[tokio::test]
    async fn message_action_routes_enforce_auth_scope_and_archive_boundaries() {
        let app = app();
        let b = bot(&app.db, "codex");
        let chat = format!("dm-{}", b.id);
        let seq = message(&app.db, &chat, &b.id, "Source");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(axum::serve(listener, web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        let url = format!("{origin}/api/chats/{chat}/messages/{seq}/reaction");
        assert_eq!(
            client
                .put(&url)
                .json(&json!({"emoji":"👍"}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&app.token)
                .json(&json!({"emoji":"👍","bot_id":b.id}))
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&app.token)
                .json(&json!({"emoji":"👍"}))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let send = format!("{origin}/api/chats/{chat}/messages");
        assert_eq!(
            client
                .post(&send)
                .bearer_auth(&app.token)
                .json(&json!({"prompt":"Reply","reply_to":seq}))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            client
                .post(&send)
                .bearer_auth(&app.token)
                .json(&json!({"prompt":"Reply","reply_to":"invalid"}))
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
        assert_eq!(
            client
                .post(&send)
                .bearer_auth(&app.token)
                .json(&json!({"prompt":"Reply","reply_to":999999}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        let mut archived = app.db.chat(&chat).unwrap();
        archived.archived = true;
        for run in app.db.runs(None).unwrap() {
            app.db.finish(&run.id, "completed", "", "").unwrap();
        }
        app.db.save_chat(&archived).unwrap();
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&app.token)
                .json(&json!({"emoji":null}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(
            client
                .post(&send)
                .bearer_auth(&app.token)
                .json(&json!({"prompt":"Reply","reply_to":seq}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        server.abort();
    }
}
