use crate::db::{BotProfile, Db};
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

pub fn muted(mutes: &Value, bot: &str, chat: &str) -> bool {
    [format!("bot:{bot}"), format!("chat:{chat}")].iter().any(|key| {
        let until = mutes[key].as_i64().unwrap_or(0);
        until == -1 || until > crate::db::now()
    })
}

impl Db {
    pub fn mute_notifications(&self, kind: &str, id: &str, seconds: i64) -> Result<Value> {
        ensure!(matches!(kind, "bot" | "chat") && !id.is_empty() && id.len() <= 160
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "Invalid notification target");
        ensure!(matches!(seconds, -1 | 0 | 3600 | 86400), "Invalid mute duration");
        let until = if seconds > 0 { crate::db::now() + seconds } else { seconds };
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let raw: Option<String> = tx.query_row("SELECT value FROM settings WHERE key='notification_mutes'", [], |r| r.get(0)).optional()?;
        let mut value: Value = raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
        value[format!("{kind}:{id}")] = json!(until);
        tx.execute("INSERT INTO settings VALUES('notification_mutes',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [value.to_string()])?;
        tx.commit()?;
        Ok(value)
    }
}

fn preview(markdown: &str) -> String {
    use pulldown_cmark::{Event, Parser, TagEnd};
    let mut text = String::new();
    for event in Parser::new(crate::runtime::bounded(markdown, 32000)) {
        match event {
            Event::Text(s) | Event::Code(s) => text.push_str(&s),
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(
                TagEnd::Paragraph | TagEnd::Item | TagEnd::Heading(_) | TagEnd::CodeBlock,
            ) => text.push(' '),
            _ => {}
        }
        if text.len() > 4000 {
            break;
        }
    }
    let plain = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result = String::new();
    for ch in plain.chars().filter(|ch| !ch.is_control()) {
        if result.len() + ch.len_utf8() > 900 {
            result.push('…');
            break;
        }
        result.push(ch);
    }
    result
}
impl Db {
    // Resolve only inside the authenticated workspace DB. Return routing metadata,
    // never bot-only conversation history or authentication instructions.
    pub fn notification_target(&self, event: &str) -> Result<Value> {
        ensure!(!event.is_empty() && event.len() <= 32 && event.bytes().all(|b| b.is_ascii_digit()), "Invalid notification event");
        let Ok(seq) = event.parse::<i64>() else { return Ok(json!({"unavailable":true})); };
        let c = self.0.lock().unwrap();
        let row: Option<(String,String,String,String,String)> = c.query_row(
            "SELECT e.kind,e.body,r.id,r.bot_id,r.chat_id FROM events e JOIN runs r ON r.id=e.run_id WHERE e.seq=?",
            [seq], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let Some((kind,body,run_id,bot_id,mut chat_id)) = row else { return Ok(json!({"unavailable":true})); };
        let body: Value = serde_json::from_str(&body)?;
        let id = body["id"].as_str().unwrap_or("");
        let sql = match kind.as_str() {
            "user_action" => Some("SELECT status FROM user_tasks WHERE id=?1 AND run_id=?2"),
            "approval" => Some("SELECT status FROM approvals WHERE id=?1 AND run_id=?2"),
            "question" => Some("SELECT status FROM questions WHERE id=?1 AND run_id=?2"),
            _ => None,
        };
        let status: Option<String> = if let Some(sql)=sql { c.query_row(sql,rusqlite::params![id,run_id],|r|r.get(0)).optional()? } else {None};
        if kind=="question" {
            if let Some(destination)=c.query_row("SELECT NULLIF(delivery_chat_id,'') FROM questions WHERE id=? AND run_id=?",rusqlite::params![id,run_id],|r|r.get::<_,Option<String>>(0)).optional()?.flatten(){chat_id=destination;}
        } else if matches!(kind.as_str(),"user_action"|"approval") {
            let hidden: bool = c.query_row("SELECT COALESCE((SELECT bot_only OR archived FROM chats WHERE id=?),1)",[&chat_id],|r|r.get(0))?;
            if hidden {chat_id=format!("dm-{bot_id}");}
        }
        Ok(json!({"bot_id":bot_id,"chat_id":chat_id,"run_id":run_id,"request_kind":kind,"request_id":if sql.is_some(){id}else{""},"status":status}))
    }
    pub fn notifications(&self, after: Option<i64>) -> Result<Value> {
        let general = self.setting("general")?.unwrap_or_default();
        let mutes = self.setting("notification_mutes")?.unwrap_or_default();
        let frequency = general["notifications"].as_str().unwrap_or("all");
        let c = self.0.lock().unwrap();
        let latest: i64 =
            c.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?;
        let Some(after) = after else {
            return Ok(json!({"cursor":latest,"items":[]}));
        };
        ensure!(after >= 0, "Invalid notification cursor");
        if frequency == "none" || after > latest {
            return Ok(json!({"cursor":latest,"items":[]}));
        }
        let rows=c.prepare("SELECT e.seq,e.kind,e.body,r.id,r.bot_id,r.chat_id,r.status,b.name,b.profile,r.output,r.error FROM events e JOIN runs r ON r.id=e.run_id JOIN bots b ON b.id=r.bot_id WHERE e.seq>? AND e.kind IN ('run_finished','approval','user_action','question','reminder') ORDER BY e.seq LIMIT 100")?.query_map([after],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,r.get::<_,String>(10)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let cursor = if rows.len() == 100 {
            rows.last().unwrap().0
        } else {
            latest
        };
        let mut items = Vec::new();
        for (seq, kind, event, run_id, bot_id, mut chat_id, status, name, profile, output, error) in
            rows
        {
            if kind=="question" {
                let event:Value=serde_json::from_str(&event)?;
                if let Some(id)=event["id"].as_str(){chat_id=c.query_row("SELECT COALESCE(NULLIF(delivery_chat_id,''),chat_id) FROM questions WHERE id=?",[id],|r|r.get(0))?;}
            }
            if matches!(kind.as_str(),"user_action"|"approval") && c.query_row("SELECT COALESCE((SELECT bot_only OR archived FROM chats WHERE id=?),1)",[&chat_id],|r|r.get::<_,bool>(0))? {chat_id=format!("dm-{bot_id}");}
            if kind=="run_finished" && status=="completed" && c.query_row("SELECT COALESCE((SELECT bot_only FROM chats WHERE id=?),0)",[&chat_id],|r|r.get::<_,bool>(0))? {continue;}
            let p: BotProfile = serde_json::from_str(&profile)?;
            if !p.notifications || p.archived || muted(&mutes, &bot_id, &chat_id) {
                continue;
            }
            let body = if kind == "reminder" {
                let event: Value = serde_json::from_str(&event)?;
                preview(&format!(
                    "Reminder: {}",
                    event["message"].as_str().unwrap_or("")
                ))
            } else if kind == "run_finished" {
                if status == "completed" {
                    if frequency == "input_needed" {
                        continue;
                    }
                    let silent:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND kind IN ('question_wait','process_wait')) OR EXISTS(SELECT 1 FROM routine_runs WHERE run_id=?1 AND quiet=1) OR EXISTS(SELECT 1 FROM events WHERE run_id=?1 AND kind='tool_result' AND json_extract(body,'$.tool')='finish_quietly' AND json_extract(body,'$.failed')=0) OR EXISTS(SELECT 1 FROM collaboration_requests WHERE parent_run_id=?1)",[&run_id],|r|r.get(0))?;
                    if silent {
                        continue;
                    }
                    let replacement =
                        crate::quiet_output::routine_replacement(&c, &run_id, &output)?;
                    let text = preview(replacement.unwrap_or(&output));
                    if text.is_empty() {
                        let files:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM attachments WHERE run_id=?1) OR EXISTS(SELECT 1 FROM deliverables WHERE run_id=?1)",[&run_id],|r|r.get(0))?;
                        if !files {
                            continue;
                        }
                        "Files attached.".to_owned()
                    } else {
                        text
                    }
                } else if matches!(status.as_str(), "failed" | "interrupted") {
                    preview(&format!(
                        "Needs help: {}",
                        if error.is_empty() {
                            "This task could not finish."
                        } else {
                            &error
                        }
                    ))
                } else {
                    continue;
                }
            } else {
                let event: Value = serde_json::from_str(&event)?;
                let id = event["id"].as_str().unwrap_or("");
                let sql = match kind.as_str() {
                    "question" => "SELECT question FROM questions WHERE id=? AND status='pending'",
                    "user_action" => {
                        "SELECT title||': '||instructions FROM user_tasks WHERE id=? AND status='pending'"
                    }
                    _ => {
                        "SELECT 'Approval requested: '||tool FROM approvals WHERE id=? AND status='pending'"
                    }
                };
                let Some(text) = c
                    .query_row(sql, [id], |r| r.get::<_, String>(0))
                    .optional()?
                else {
                    continue;
                };
                preview(&text)
            };
            let identity = json!([name, p.shape, p.color, p.eyes]).to_string();
            let avatar_key = ring::digest::digest(&ring::digest::SHA256, identity.as_bytes())
                .as_ref()
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect::<String>();
            items.push(json!({"id":seq,"run_id":run_id,"bot_id":bot_id,"chat_id":chat_id,"title":name,"body":body,"avatar_key":avatar_key,"avatar":{"name":name,"shape":p.shape,"color":p.color,"eyes":p.eyes,"animated":p.animated},"reduced_motion":general["reduced_motion"]==true}));
        }
        Ok(json!({"cursor":cursor,"items":items}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn event_target_requires_workspace_token_and_returns_only_routing_metadata() {
        use tower::ServiceExt;
        let app=crate::tests::app();let bot=crate::tests::bot(&app.db,"codex");app.db.queue(&bot.id,"Request",0).unwrap();let run=app.db.claim_bot(&bot.id).unwrap().unwrap();
        let task=app.db.request_user_task(&run,"Sign in","Private instructions must not be in route metadata").unwrap();
        let seq:i64=app.db.0.lock().unwrap().query_row("SELECT seq FROM events WHERE run_id=? AND kind='user_action'",[&run.id],|r|r.get(0)).unwrap();
        let router=crate::web::router(app.clone());
        for token in ["","wrong-workspace",app.token.as_str()] {
            let response=router.clone().oneshot(axum::http::Request::builder().uri(format!("/api/notification-target/{seq}")).header("Authorization",format!("Bearer {token}")).body(axum::body::Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status().as_u16(),if token==app.token{200}else{401});
            if token==app.token {let bytes=axum::body::to_bytes(response.into_body(),4096).await.unwrap();let value:Value=serde_json::from_slice(&bytes).unwrap();assert_eq!(value["request_id"],task.id);assert_eq!(value["status"],"pending");assert!(!String::from_utf8(bytes.to_vec()).unwrap().contains("Private instructions"));}
        }
    }
    #[test]
    fn helper_request_routes_to_direct_chat_and_stale_event_keeps_receipt() {
        let db=Db::open(":memory:").unwrap();let a=crate::tests::bot(&db,"codex");let b=crate::tests::bot(&db,"codex");
        let parent=db.queue(&a.id,"Parent",0).unwrap();let parent=db.run(&parent).unwrap();
        let child=db.chat_handoff(&parent,&b.id,"Help").unwrap();
        db.0.lock().unwrap().execute("UPDATE runs SET status='running' WHERE id=?",[&child]).unwrap();
        let task=db.request_user_task(&db.run(&child).unwrap(),"Sign in","Disposable fixture only").unwrap();
        let seq:i64=db.0.lock().unwrap().query_row("SELECT seq FROM events WHERE kind='user_action' AND run_id=?",[&child],|r|r.get(0)).unwrap();
        let target=db.notification_target(&seq.to_string()).unwrap();assert_eq!(target["chat_id"],format!("dm-{}",b.id));assert_eq!(target["request_id"],task.id);assert_eq!(target["status"],"pending");
        let items=db.notifications(Some(seq-1)).unwrap();assert_eq!(items["items"][0]["chat_id"],target["chat_id"]);
        db.0.lock().unwrap().execute("UPDATE user_tasks SET status='expired' WHERE id=?",[&task.id]).unwrap();
        assert_eq!(db.notification_target(&seq.to_string()).unwrap()["status"],"expired");assert!(db.notifications(Some(seq-1)).unwrap()["items"].as_array().unwrap().is_empty());
        db.0.lock().unwrap().execute("UPDATE runs SET status='running' WHERE id=?",[&child]).unwrap();
        let approval=db.request_approval(&child,"computer_open_url",&json!({"url":"https://fixture.invalid"})).unwrap();db.event(&child,"approval",json!({"id":approval})).unwrap();
        let approval_seq:i64=db.0.lock().unwrap().query_row("SELECT MAX(seq) FROM events WHERE run_id=?",[&child],|r|r.get(0)).unwrap();let approval_target=db.notification_target(&approval_seq.to_string()).unwrap();assert_eq!(approval_target["request_kind"],"approval");assert_eq!(approval_target["request_id"],approval);assert_eq!(approval_target["chat_id"],target["chat_id"]);db.decide(&approval,false).unwrap();assert_eq!(db.notification_target(&approval_seq.to_string()).unwrap()["status"],"denied");
        assert_eq!(Db::open(":memory:").unwrap().notification_target(&seq.to_string()).unwrap()["unavailable"],true);
        assert_eq!(db.notification_target("99999999999999999999999999999999").unwrap()["unavailable"],true);assert!(db.notification_target("../123").is_err());
    }
    #[test]
    fn preview_keeps_message_without_markdown_or_link_destinations() {
        assert_eq!(
            preview("**Hello**, [Alex](https://private.test).\n`ready` <img src=x>"),
            "Hello, Alex. ready"
        );
        let text = preview(&"🦉".repeat(2000));
        assert!(text.ends_with('…'));
        assert!(text.len() <= 903);
    }
}
