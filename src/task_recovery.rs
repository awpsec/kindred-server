//! User-requested recovery has durable identity, separate from chat presentation.
use crate::db::{self, Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub fn metadata(c: &Connection, run: &str, kind: &str) -> Result<Option<Value>> {
    let body: Option<String> = c
        .query_row(
            "SELECT body FROM events WHERE run_id=? AND kind=? ORDER BY seq DESC LIMIT 1",
            params![run, kind],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body).map_err(Into::into))
        .transpose()
}

// Only migrate the exact former UI-generated prompt with its durable receipt.
// Ordinary user messages mentioning continuation are never reclassified.
pub fn repair_legacy(c: &Connection) -> Result<()> {
    let rows = c.prepare("SELECT old.id,old.prompt,next.id,next.prompt,m.seq FROM chat_send_receipts receipt JOIN runs old ON old.id=receipt.request_id JOIN runs next ON next.id=json_extract(receipt.runs,'$[0]') AND next.bot_id=old.bot_id AND next.chat_id=old.chat_id JOIN run_message_sources source ON source.run_id=next.id JOIN chat_messages m ON m.seq=source.message_seq AND m.kind='message' AND m.sender='user' AND m.body=next.prompt WHERE json_array_length(receipt.runs)=1 ORDER BY m.seq")?
        .query_map([], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (source, original, next, prompt, seq) in rows {
        let excerpt = String::from_utf8_lossy(&original.as_bytes()[..original.len().min(56000)]);
        let expected = format!(
            "Continue the unfinished work from task {source}. Review its completed actions and current state before taking the next step. Do not repeat an external write with an uncertain outcome, override a declined action, or recreate existing routines. Preserve the original scope. Original request:\n{excerpt}"
        );
        if prompt != expected {
            continue;
        }
        let previous = metadata(c, &source, "task_recovery")?;
        let root = previous
            .as_ref()
            .and_then(|v| v["root_run_id"].as_str())
            .unwrap_or(&source);
        for (owner, kind, body) in [
            (source.as_str(), "task_continued", json!({"run_id":next})),
            (
                next.as_str(),
                "task_recovery",
                json!({"source_run_id":source,"root_run_id":root}),
            ),
        ] {
            if metadata(c, owner, kind)?.is_none() {
                c.execute(
                    "INSERT INTO events(run_id,kind,body,created) VALUES(?,?,?,?)",
                    params![owner, kind, body.to_string(), db::now()],
                )?;
            }
        }
        c.execute("UPDATE chat_messages SET kind='continuation',body='Continuing task',run_id=? WHERE seq=?",params![next,seq])?;
    }
    Ok(())
}

impl Db {
    pub fn continue_task(&self, source: &str) -> Result<String> {
        let run = self.run(source)?;
        let mut chat = self.chat(&run.chat_id)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        // Receipt and queue/message writes share one transaction. An uncertain
        // HTTP response or simultaneous click can never start a second retry.
        if let Some(receipt) = metadata(&tx, source, "task_continued")? {
            return Ok(receipt["run_id"].as_str().unwrap_or_default().to_owned());
        }
        ensure!(
            !crate::workspace_transfer::frozen(&tx)?,
            "This workspace is being moved"
        );
        let status: String =
            tx.query_row("SELECT status FROM runs WHERE id=?", [source], |r| r.get(0))?;
        ensure!(
            matches!(status.as_str(), "failed" | "interrupted" | "cancelled"),
            "Only a stopped task can be continued"
        );
        let (members, archived): (String, bool) = tx.query_row(
            "SELECT members,archived FROM chats WHERE id=?",
            [&chat.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(!archived, "This chat is archived");
        chat.members = serde_json::from_str(&members)?;
        ensure!(chat.members.iter().any(|id| id == &run.bot_id), "Teammate is not in this active chat");
        let profile: String=tx.query_row("SELECT profile FROM bots WHERE id=?",[&run.bot_id],|r|r.get(0))?;
        ensure!(serde_json::from_str::<Value>(&profile)?["archived"] != true, "This bot is archived");
        let previous = metadata(&tx, source, "task_recovery")?;
        let root = previous
            .as_ref()
            .and_then(|v| v["root_run_id"].as_str())
            .unwrap_or(source);
        let original: String = tx.query_row(
            "SELECT prompt FROM runs WHERE id=? AND bot_id=? AND chat_id=?",
            params![root, run.bot_id, run.chat_id],
            |r| r.get(0),
        )?;
        // Keep even a maximum-length original request intact. Recovery rules and
        // source activity are supplied separately in the shared instruction packet.
        // A fresh user follow-up after Stop may already be handling the handoff.
        // Reuse only the immediate next turn for this bot, in the same chat;
        // never absorb a routine, command, teammate message or completed work.
        let followup: Option<String> = if status == "cancelled" {
            tx.query_row("SELECT r.id FROM runs r JOIN run_message_sources s ON s.run_id=r.id JOIN chat_messages m ON m.seq=s.message_seq WHERE r.bot_id=?1 AND r.chat_id=?2 AND r.status IN ('queued','running','awaiting_user','awaiting_approval') AND r.created>=?3 AND EXISTS(SELECT 1 FROM events WHERE run_id=?4 AND kind='run_stop_requested' AND created>=?5) AND r.rowid=(SELECT MIN(rowid) FROM runs WHERE bot_id=?1 AND rowid>(SELECT rowid FROM runs WHERE id=?4)) AND r.depth=0 AND r.reply_to='' AND m.sender='user' AND m.kind='message' AND m.suppressed=0 AND NOT EXISTS(SELECT 1 FROM run_commands WHERE run_id=r.id) AND NOT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=r.id) AND NOT EXISTS(SELECT 1 FROM events WHERE run_id=r.id AND kind='task_recovery')",params![run.bot_id,run.chat_id,db::now()-120,source,db::now()-900],|r|r.get(0)).optional()?
        } else { None };
        let merged = followup.is_some();
        let next = match followup {
            Some(id) => id,
            None => crate::chats::insert_run(&tx, &chat, &run.bot_id, &original, &db::id(), "", 0)?,
        };
        tx.execute("INSERT INTO run_commands(run_id,receipt) SELECT ?,receipt FROM run_commands WHERE run_id=?", params![next,root])?;
        if !merged {
            tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,'user','Continuing task','continuation',?,?)", params![chat.id,next,db::now()])?;
            let seq = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO run_message_sources VALUES(?,?)",
                params![next, seq],
            )?;
            tx.execute("INSERT INTO message_replies(message_seq,reply_to_seq) SELECT ?,p.reply_to_seq FROM message_replies p JOIN run_message_sources s ON s.message_seq=p.message_seq WHERE s.run_id=?", params![seq,root])?;
        }
        for (owner, kind, body) in [
            (source, "task_continued", json!({"run_id":next})),
            (
                next.as_str(),
                "task_recovery",
                json!({"source_run_id":source,"root_run_id":root,"merged_followup":merged}),
            ),
        ] {
            tx.execute(
                "INSERT INTO events(run_id,kind,body,created) VALUES(?,?,?,?)",
                params![owner, kind, body.to_string(), db::now()],
            )?;
        }
        tx.commit()?;
        Ok(next)
    }

    pub fn task_recovery(&self, run: &Run) -> Result<Option<Value>> {
        metadata(&self.0.lock().unwrap(), &run.id, "task_recovery")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests;
    use std::future::IntoFuture;

    #[test]
    fn continue_reuses_followup_without_requeueing_or_rewriting_user_note() {
        for running in [false,true] {
            let app=tests::app();let bot=tests::bot(&app.db,"codex");
            let source=app.db.queue(&bot.id,"Finish the original task",0).unwrap();
            app.db.cancel(&source).unwrap();
            app.db.finish(&source,"cancelled","","Stopped").unwrap();
            let followup=app.db.queue(&bot.id,"I signed in for you; carry on",0).unwrap();
            if running {assert_eq!(app.db.claim_bot(&bot.id).unwrap().unwrap().id,followup);}
            let before=app.db.run(&followup).unwrap();
            assert_eq!(app.db.continue_task(&source).unwrap(),followup);
            assert_eq!(app.db.continue_task(&source).unwrap(),followup);
            let after=app.db.run(&followup).unwrap();
            assert_eq!(before.prompt,after.prompt);assert_eq!(before.status,after.status);
            assert_eq!(app.db.runs(None).unwrap().len(),2);
            assert_eq!(app.db.task_recovery(&after).unwrap().unwrap()["root_run_id"],source);
            assert!(!app.db.chat_messages(&after.chat_id).unwrap().iter().any(|m|m["kind"]=="continuation"));
            if running {
                let value=crate::conversation_updates::with_live_context(&app.db,&after,json!({"text":"Screen inspected"})).unwrap();
                let context:Value=serde_json::from_str(value["text"].as_str().unwrap()).unwrap();
                assert_eq!(context["kindred_live_context"]["task_recovery"]["original_request"],"Finish the original task");
                let second=crate::conversation_updates::with_live_context(&app.db,&after,json!({"text":"Next result"})).unwrap();
                assert!(!second.to_string().contains("merged_followup"));
            }
        }
    }
    #[test]
    fn continue_does_not_absorb_completed_or_stale_followups() {
        for stale in [false,true] {
            let app=tests::app();let bot=tests::bot(&app.db,"codex");
            let source=app.db.queue(&bot.id,"Original",0).unwrap();
            app.db.cancel(&source).unwrap();
            app.db.finish(&source,"cancelled","","Stopped").unwrap();
            let note=app.db.queue(&bot.id,"A separate message",0).unwrap();
            if stale {app.db.0.lock().unwrap().execute("UPDATE runs SET created=created-300 WHERE id=?",[&note]).unwrap();}
            else {app.db.finish(&note,"completed","Done","").unwrap();}
            assert_ne!(app.db.continue_task(&source).unwrap(),note);
        }
    }

    #[tokio::test]
    async fn continuation_http_is_authenticated_atomic_and_idempotent() {
        let app = tests::app();
        let bot = tests::bot(&app.db, "codex");
        let source = app
            .db
            .queue(&bot.id, "Send the reviewed report once", 0)
            .unwrap();
        app.db
            .event(
                &source,
                "assistant",
                json!({"text":"Provider disconnected","status_notice":"provider_error"}),
            )
            .unwrap();
        app.db
            .finish(&source, "failed", "", "Connection lost")
            .unwrap();
        app.db.chat_complete(&app.db.run(&source).unwrap()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/runs/{source}/continue",
            listener.local_addr().unwrap()
        );
        let server =
            tokio::spawn(axum::serve(listener, crate::web::router(app.clone())).into_future());
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .post(&url)
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let send = || {
            client
                .post(&url)
                .bearer_auth(&app.token)
                .json(&json!({}))
                .send()
        };
        let (a, b) = tokio::join!(send(), send());
        let a: Value = a.unwrap().error_for_status().unwrap().json().await.unwrap();
        let b: Value = b.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(a, b);
        let next = app.db.run(a["run_id"].as_str().unwrap()).unwrap();
        assert_eq!(next.prompt, "Send the reviewed report once");
        assert!(app.db.continue_task(&next.id).is_err());
        let messages = app.db.chat_messages(&next.chat_id).unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|m| m["kind"] == "continuation")
                .count(),
            1
        );
        assert!(
            messages
                .iter()
                .filter(|m| m["status_notice"].is_object())
                .all(|m| m["status_notice"]["continued_by"] == next.id)
        );
        assert_eq!(messages.last().unwrap()["text"], "Continuing task");
        assert_eq!(app.db.runs(None).unwrap().len(), 2);
        server.abort();
    }

    #[test]
    fn repeated_failure_keeps_full_request_context_and_archived_bots_cannot_restart() {
        let app = tests::app();
        let mut bot = tests::bot(&app.db, "codex");
        let original = "x".repeat(64000);
        let source = app.db.queue(&bot.id, &original, 0).unwrap();
        app.db
            .event(
                &source,
                "tool_result",
                json!({"tool":"guest_exec","text":"Already sent report","failed":false}),
            )
            .unwrap();
        app.db
            .finish(&source, "failed", "", "Connection lost")
            .unwrap();
        let next = app.db.continue_task(&source).unwrap();
        let run = app.db.run(&next).unwrap();
        let packet = crate::instructions::build(&app, &bot, &run, &[], None).unwrap();
        assert!(packet.contains("Already sent report"));
        assert!(packet.contains("uncertain outcome"));
        assert_eq!(run.prompt, original);
        app.db
            .finish(&next, "interrupted", "", "Interrupted")
            .unwrap();
        let third = app.db.continue_task(&next).unwrap();
        assert_eq!(app.db.run(&third).unwrap().prompt, original);
        assert_eq!(
            app.db
                .task_recovery(&app.db.run(&third).unwrap())
                .unwrap()
                .unwrap()["root_run_id"],
            source
        );
        app.db.finish(&third, "failed", "", "Unavailable").unwrap();
        bot.profile.archived = true;
        app.db.save_bot(&bot).unwrap();
        assert!(app.db.continue_task(&third).is_err());
        assert!(app.db.continue_task("not-in-this-workspace").is_err());
    }

    #[test]
    fn legacy_receipt_projection_repairs_without_changing_run_history() {
        let db = Db::open(":memory:").unwrap();
        let bot = tests::bot(&db, "codex");
        let source = db.queue(&bot.id, "Original request", 0).unwrap();
        db.finish(&source, "failed", "", "Disconnected").unwrap();
        let prompt = format!(
            "Continue the unfinished work from task {source}. Review its completed actions and current state before taking the next step. Do not repeat an external write with an uncertain outcome, override a declined action, or recreate existing routines. Preserve the original scope. Original request:\nOriginal request"
        );
        let chat = format!("dm-{}", bot.id);
        let next = db
            .chat_send_request(&chat, &prompt, &[], &[], None, Some(&source))
            .unwrap()[0]
            .clone();
        let seq = db.chat_messages(&chat).unwrap().last().unwrap()["seq"]
            .as_i64()
            .unwrap();
        let reply = db
            .chat_send_message(&chat, "One clarification", &[], &[], Some(seq))
            .unwrap()[0]
            .clone();
        repair_legacy(&db.0.lock().unwrap()).unwrap();
        repair_legacy(&db.0.lock().unwrap()).unwrap();
        assert_eq!(db.run(&next).unwrap().prompt, prompt);
        assert_eq!(
            db.chat_messages(&chat)
                .unwrap()
                .iter()
                .find(|m| m["seq"] == seq)
                .unwrap()["text"],
            "Continuing task"
        );
        assert_eq!(
            db.quoted_context_for_run(&db.run(&reply).unwrap())
                .unwrap()
                .unwrap()["text"],
            "Continuing task"
        );
        assert_eq!(db.continue_task(&source).unwrap(), next);
        assert_eq!(
            db.events(&next)
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "task_recovery")
                .count(),
            1
        );
    }

    #[test]
    fn source_files_quotes_and_decisions_survive_recovery_and_it_cannot_be_steered() {
        let app = tests::app();
        let bot = tests::bot(&app.db, "codex");
        let earlier = app.db.queue(&bot.id, "Use the reviewed draft", 0).unwrap();
        let chat = format!("dm-{}", bot.id);
        app.db
            .finish(&earlier, "completed", "Reviewed draft", "")
            .unwrap();
        app.db
            .chat_complete(&app.db.run(&earlier).unwrap())
            .unwrap();
        let quote = app.db.chat_messages(&chat).unwrap().last().unwrap()["seq"]
            .as_i64()
            .unwrap();
        let file = app.db.upload_file(&chat, "report.txt", "cmVwb3J0").unwrap();
        let source = app
            .db
            .chat_send_message(
                &chat,
                "Finish the report",
                &[],
                &[file["id"].as_str().unwrap().into()],
                Some(quote),
            )
            .unwrap()[0]
            .clone();
        let running = app.db.claim().unwrap().unwrap();
        assert_eq!(running.id, source);
        let approval = app
            .db
            .request_approval(
                &source,
                "send_email",
                &json!({"subject":"Do not send this"}),
            )
            .unwrap();
        app.db.decide(&approval, false).unwrap();
        app.db.0.lock().unwrap().execute("INSERT INTO questions(id,run_id,chat_id,bot_id,topic_key,question,context,options,status,answer,continuation_run_id,created) VALUES('saved',?,?,?,'delivery','Send the report?','','[]','answered','Keep it as a draft',?,?)",params![source,chat,bot.id,source,db::now()]).unwrap();
        app.db
            .finish(&source, "failed", "", "Disconnected")
            .unwrap();
        let next = app.db.continue_task(&source).unwrap();
        let run = app.db.run(&next).unwrap();
        assert_eq!(
            app.db.quoted_context_for_run(&run).unwrap().unwrap()["seq"],
            quote
        );
        assert_eq!(
            app.db.decisions_for_run(&run, None).unwrap()[0]["is_current_continuation"],
            true
        );
        let packet = crate::instructions::build(&app, &bot, &run, &[], None).unwrap();
        assert!(
            packet.contains("report.txt")
                && packet.contains("Keep it as a draft")
                && packet.contains("denied")
        );
        let busy = app.db.queue(&bot.id, "Unrelated work", 0).unwrap();
        app.db
            .0
            .lock()
            .unwrap()
            .execute("UPDATE runs SET status='running' WHERE id=?", [&busy])
            .unwrap();
        assert!(app.db.request_steering(&next, &busy).is_err());
    }

    #[test]
    fn receipt_and_compact_message_survive_database_reopen() {
        let folder = std::env::temp_dir().join(format!("kindred-task-recovery-{}", db::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("test.db");
        let (source, next, chat) = {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            let bot = tests::bot(&db, "codex");
            let source = db.queue(&bot.id, "Original task", 0).unwrap();
            db.finish(&source, "failed", "", "Unavailable").unwrap();
            let next = db.continue_task(&source).unwrap();
            (source, next, format!("dm-{}", bot.id))
        };
        let db = Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(db.continue_task(&source).unwrap(), next);
        assert_eq!(
            db.chat_messages(&chat)
                .unwrap()
                .iter()
                .filter(|m| m["kind"] == "continuation")
                .count(),
            1
        );
        drop(db);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
