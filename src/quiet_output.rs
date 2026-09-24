//! Keep serialized quiet-completion markers out of conversation presentation.
//! This does not execute a tool or mark an inbox check as successful. Raw provider
//! output and tool receipts remain available in the run history.
use anyhow::Result;
use rusqlite::Connection;

pub fn is_marker(text: &str) -> bool {
    let text = text.trim();
    if text.len() > 256 {
        return false;
    }
    ["finish_quietly", "mcp__kindred__finish_quietly"]
        .iter()
        .any(|name| {
            if let Some(inner) = text
                .strip_prefix(&format!("<{name}>"))
                .and_then(|s| s.strip_suffix(&format!("</{name}>")))
            {
                return inner.trim().is_empty() || inner.trim() == "{}";
            }
            text == format!("<{name}/>") || text == format!("<{name} />")
        })
}

// None means an ordinary reply. Some("") means no message; a nonempty replacement
// preserves failed checks or file delivery instead of showing a blank bubble.
pub fn routine_replacement(c: &Connection, run: &str, text: &str) -> Result<Option<&'static str>> {
    if !is_marker(text) {
        return Ok(None);
    }
    let routine: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM routine_runs WHERE run_id=?)",
        [run],
        |r| r.get(0),
    )?;
    if !routine {
        return Ok(None);
    }
    let failed: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id=? AND ((kind='tool_result' AND json_extract(body,'$.failed')=1) OR (kind='assistant' AND json_extract(body,'$.status_notice')='provider_error')))",
        [run], |r| r.get(0),
    )?;
    if failed {
        return Ok(Some(
            "This routine encountered a tool or provider error and returned no readable summary. Open task details to review the failed check.",
        ));
    }
    let files: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachments WHERE run_id=?1) OR EXISTS(SELECT 1 FROM deliverables WHERE run_id=?1)",
        [run], |r| r.get(0),
    )?;
    Ok(Some(if files { "Files attached." } else { "" }))
}

pub fn repair(c: &Connection) -> Result<()> {
    let rows = c.prepare("SELECT m.seq,m.run_id,m.body FROM chat_messages m JOIN runs r ON r.id=m.run_id WHERE m.suppressed=0 AND m.kind='result' AND r.status='completed' AND r.error='' AND EXISTS(SELECT 1 FROM routine_runs rr WHERE rr.run_id=r.id) AND m.body LIKE '%finish_quietly%'")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seq, run, body) in rows {
        if let Some(replacement) = routine_replacement(c, &run, &body)? {
            if replacement.is_empty() {
                c.execute("UPDATE chat_messages SET suppressed=1 WHERE seq=?", [seq])?;
            } else {
                c.execute(
                    "UPDATE chat_messages SET body=? WHERE seq=?",
                    rusqlite::params![replacement, seq],
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{self, Db},
        tests::bot,
    };
    use rusqlite::params;
    use serde_json::json;

    #[test]
    fn only_empty_unquoted_completion_tags_are_markers() {
        for text in [
            "<mcp__kindred__finish_quietly> </mcp__kindred__finish_quietly>",
            "\n<finish_quietly>{}</finish_quietly>\n",
            "<finish_quietly />",
            "<mcp__kindred__finish_quietly/>",
        ] {
            assert!(is_marker(text), "{text}");
        }
        for text in [
            "",
            "finish_quietly",
            "<other/>",
            "`<finish_quietly/>`",
            "```xml\n<finish_quietly/>\n```",
            "Use <finish_quietly/> to finish.",
            "<finish_quietly>Inbox failed</finish_quietly>",
            "<finish_quietly/><finish_quietly/>",
            "<finish_quietly reason='error'/>",
        ] {
            assert!(!is_marker(text), "{text}");
        }
    }

    #[test]
    fn routine_markers_do_not_deliver_notify_or_change_unread_history() {
        let db = Db::open(":memory:").unwrap();
        let bot = bot(&db, "claude-code");
        let chat = format!("dm-{}", bot.id);
        let normal = db.queue(&bot.id, "Check inbox", 0).unwrap();
        db.finish(&normal, "completed", "A client needs a reply.", "")
            .unwrap();
        db.chat_complete(&db.run(&normal).unwrap()).unwrap();
        let seq = db.chat_messages(&chat).unwrap().last().unwrap()["seq"]
            .as_i64()
            .unwrap();
        db.mark_chat_read(&chat, seq).unwrap();
        let cursor = db.notifications(None).unwrap()["cursor"].as_i64().unwrap();
        let initial_messages = db.chat_messages(&chat).unwrap().len();
        for marker in [
            "<mcp__kindred__finish_quietly> </mcp__kindred__finish_quietly>",
            "<finish_quietly>{}</finish_quietly>",
            "<finish_quietly />",
            "<mcp__kindred__finish_quietly/>",
        ] {
            let chat_row = db.chat(&chat).unwrap();
            let id = crate::chats::insert_run(
                &db.0.lock().unwrap(),
                &chat_row,
                &bot.id,
                "Repeated inbox check",
                &db::id(),
                "",
                0,
            )
            .unwrap();
            db.0.lock()
                .unwrap()
                .execute("INSERT INTO routine_runs VALUES(?,'fixture',0)", [&id])
                .unwrap();
            db.event(&id, "assistant", json!({"text":marker})).unwrap();
            db.finish(&id, "completed", marker, "").unwrap();
            db.event(&id, "run_finished", json!({})).unwrap();
            db.chat_complete(&db.run(&id).unwrap()).unwrap();
            assert!(
                !db.chat_messages(&chat)
                    .unwrap()
                    .iter()
                    .any(|m| m["run_id"] == id)
            );
            assert_eq!(db.run(&id).unwrap().output, marker);
            assert_eq!(
                db.events(&id)
                    .unwrap()
                    .iter()
                    .filter(|e| e["kind"] == "tool_result")
                    .count(),
                0,
                "Text must not execute a tool"
            );
            assert_eq!(
                db.0.lock()
                    .unwrap()
                    .query_row(
                        "SELECT quiet FROM routine_runs WHERE run_id=?",
                        [&id],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        assert_eq!(db.chat_messages(&chat).unwrap().len(), initial_messages);
        assert_eq!(
            db.chat_message_page(&chat, None, None, 10).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            initial_messages
        );
        assert_eq!(
            db.chats().unwrap()[0].last_message.as_ref().unwrap()["text"],
            "A client needs a reply."
        );
        assert_eq!(db.attention().unwrap()["chats"][&chat]["unread"], false);
        assert!(
            db.notifications(Some(cursor)).unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn repair_preserves_errors_files_quotes_and_raw_records_across_restarts() {
        let root = std::env::temp_dir().join(format!("kindred-quiet-output-{}", db::id()));
        let path = root.join("kindred.db");
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let bot = bot(&db, "claude-code");
        let chat = format!("dm-{}", bot.id);
        let marker = "<mcp__kindred__finish_quietly>{}</mcp__kindred__finish_quietly>";
        let mut cases = vec![];
        for (case, text) in [
            ("quiet", marker),
            ("failure", marker),
            ("tool_failure", marker),
            ("files", marker),
            ("quoted", "`<finish_quietly/>`"),
            ("real", "A reply is needed. <finish_quietly/>"),
            ("ordinary", marker),
        ] {
            let id = db.queue(&bot.id, "Check inbox", 0).unwrap();
            if case != "ordinary" {
                db.0.lock()
                    .unwrap()
                    .execute("INSERT INTO routine_runs VALUES(?,'fixture',0)", [&id])
                    .unwrap();
            }
            db.event(&id, "assistant", json!({"text":text})).unwrap();
            if case == "tool_failure" {
                db.event(
                    &id,
                    "tool_result",
                    json!({"tool":"claude_connector","failed":true,"text":"Gmail access denied"}),
                )
                .unwrap();
            }
            if case == "files" {
                db.attach_screenshot(&id, "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=", "Report").unwrap();
            }
            db.finish(
                &id,
                if case == "failure" {
                    "failed"
                } else {
                    "completed"
                },
                text,
                if case == "failure" {
                    "Gmail access denied"
                } else {
                    ""
                },
            )
            .unwrap();
            db.event(&id, "run_finished", json!({})).unwrap();
            db.chat_complete(&db.run(&id).unwrap()).unwrap();
            // Reproduce the old marker row without deleting any original record.
            if case == "quiet" {
                db.0.lock().unwrap().execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'result',?,1)", params![chat, bot.id, text, id]).unwrap();
            }
            cases.push((case, id));
        }
        let raw_count: i64 =
            db.0.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM chat_messages", [], |r| r.get(0))
                .unwrap();
        drop(db);
        for _ in 0..2 {
            let db = Db::open(path.to_str().unwrap()).unwrap();
            let messages = db.chat_messages(&chat).unwrap();
            let notifications = db.notifications(Some(0)).unwrap();
            for (case, id) in &cases {
                let message = messages
                    .iter()
                    .find(|m| m["run_id"] == *id && m["kind"] == "result");
                let notification = notifications["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|n| n["run_id"] == *id);
                match *case {
                    "quiet" => {
                        assert!(message.is_none());
                        assert!(notification.is_none());
                    }
                    "failure" => {
                        assert!(
                            message.unwrap()["text"]
                                .as_str()
                                .unwrap()
                                .contains("Gmail access denied")
                        );
                        assert!(notification.is_some());
                    }
                    "tool_failure" => {
                        assert!(
                            message.unwrap()["text"]
                                .as_str()
                                .unwrap()
                                .contains("failed check")
                        );
                        assert!(notification.is_some());
                    }
                    "files" => {
                        assert_eq!(message.unwrap()["text"], "Files attached.");
                        assert_eq!(message.unwrap()["attachments"].as_array().unwrap().len(), 1);
                        assert!(notification.is_some());
                    }
                    _ => assert!(message.is_some()),
                }
                assert!(!db.events(id).unwrap().is_empty());
                if ["quiet", "tool_failure", "files"].contains(case) {
                    assert_eq!(db.run(id).unwrap().output, marker);
                }
            }
            assert_eq!(
                db.0.lock()
                    .unwrap()
                    .query_row("SELECT count(*) FROM chat_messages", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                raw_count
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
