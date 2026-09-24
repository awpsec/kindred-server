//! A paged index of saved bot output. Listing never reads file bytes or list items.
use crate::{db::Db, runtime::Shared};
use anyhow::{Result, ensure};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

const PAGE: usize = 20;
const SNIPPET_PREDICATE: &str = "kind IN ('result','assistant') AND (instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'```html'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'```jsx'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'```react'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'```shard-html'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'```shard-jsx'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'~~~html'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'~~~jsx'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'~~~react'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'~~~shard-html'||char(10))>0 OR instr(lower(replace(replace(replace(body,char(13),''),char(9),''),' ','')),'~~~shard-jsx'||char(10))>0)";
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE INDEX IF NOT EXISTS checklists_artifacts ON checklists(bot_id,created DESC,id DESC);
        CREATE INDEX IF NOT EXISTS reminders_artifacts ON reminders(bot_id,created DESC,id DESC);
        CREATE INDEX IF NOT EXISTS deliverables_artifacts ON deliverables(created DESC,id DESC);
        CREATE INDEX IF NOT EXISTS runs_artifact_bot ON runs(bot_id,id);")?;
    // A partial SQLite index covers old and new chat output without a second
    // copy of its source, an eager history download, or a background scan loop.
    c.execute_batch(&format!("CREATE INDEX IF NOT EXISTS chat_inline_shards ON chat_messages(sender,created DESC,seq DESC) WHERE {SNIPPET_PREDICATE}; DROP INDEX IF EXISTS chat_inline_artifacts;"))?;
    Ok(())
}
#[derive(Default, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub before: String,
    #[serde(default)]
    pub kind: String,
}
impl Db {
    pub fn artifact_page(&self, bot: &str, query: &PageQuery) -> Result<Value> {
        self.bot(bot)?;
        ensure!(
            matches!(
                query.kind.as_str(),
                "" | "file" | "checklist" | "reminder" | "snippet" | "visual" | "workspace"
            ),
            "Unknown artifact kind"
        );
        ensure!(query.before.len() <= 512, "Invalid artifact cursor");
        let (at, kind, id): (i64, String, String) = if query.before.is_empty() {
            (i64::MAX, String::new(), String::new())
        } else {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&query.before)?)?
        };
        let c = self.0.lock().unwrap();
        let mut items: Vec<Value> = Vec::new();
        for (label, from, owner, title, status, extra) in [
            ("workspace", "(SELECT *,updated AS created FROM workspace_artifacts) a", "a.bot_id", "json_extract(a.body,'$.title')", "CASE WHEN a.archived OR a.updated<=unixepoch()-1209600 THEN 'archived' ELSE 'active' END", "a.chat_id,0,''"),
            (
                "visual",
                "(SELECT v.*,m.created FROM visual_panels v JOIN chat_messages m ON m.seq=v.message_seq WHERE m.suppressed=0) a",
                "a.bot_id",
                "json_extract(a.body,'$.title')",
                "json_extract(a.body,'$.kind')",
                "a.chat_id,0,COALESCE(json_extract(a.body,'$.source_url'),'')",
            ),
            (
                "file",
                "deliverables a JOIN runs r ON r.id=a.run_id",
                "r.bot_id",
                "a.name",
                "''",
                "r.chat_id,length(a.bytes),a.source_url",
            ),
            (
                "checklist",
                "checklists a",
                "a.bot_id",
                "json_extract(a.body,'$.title')",
                "CASE WHEN json_extract(a.body,'$.archived') THEN 'archived' ELSE 'active' END",
                "a.chat_id,0,''",
            ),
            (
                "reminder",
                "reminders a",
                "a.bot_id",
                "substr(json_extract(a.body,'$.message'),1,240)",
                "a.status",
                "a.chat_id,0,''",
            ),
        ] {
            if !query.kind.is_empty() && query.kind != label {
                continue;
            }
            let sql = format!(
                "SELECT a.id,{title},a.created,{status},{extra} FROM {from} WHERE {owner}=?1 AND
                (a.created<?2 OR (a.created=?2 AND (?3<?4 OR (?3=?4 AND a.id<?5))))
                ORDER BY a.created DESC,a.id DESC LIMIT 21"
            );
            let rows = c.prepare(&sql)?.query_map(params![bot, at, label, kind, id], |r| Ok(json!({
                "id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"created":r.get::<_,i64>(2)?,
                "status":r.get::<_,String>(3)?,"chat_id":r.get::<_,String>(4)?,"size":r.get::<_,i64>(5)?,
                "source_url":r.get::<_,String>(6)?,"kind":label
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            items.extend(rows);
        }
        if query.kind.is_empty() || query.kind == "snippet" {
            let sql = format!(
                "SELECT seq,chat_id,created FROM chat_messages WHERE sender=?1 AND {SNIPPET_PREDICATE} AND (created<?2 OR (created=?2 AND ('snippet'<?3 OR ('snippet'=?3 AND printf('%020d',seq)<?4)))) ORDER BY created DESC,seq DESC LIMIT 21"
            );
            items.extend(c.prepare(&sql)?.query_map(params![bot,at,kind,id],|r|Ok(json!({"id":format!("{:020}",r.get::<_,i64>(0)?),"chat_id":r.get::<_,String>(1)?,"created":r.get::<_,i64>(2)?,"kind":"snippet","title":"Interactive shard from chat","status":"","size":0})))?.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        items.sort_by(|a, b| {
            (b["created"].as_i64(), b["kind"].as_str(), b["id"].as_str()).cmp(&(
                a["created"].as_i64(),
                a["kind"].as_str(),
                a["id"].as_str(),
            ))
        });
        let more = items.len() > PAGE;
        items.truncate(PAGE);
        let cursor = if more {
            let last = items.last().unwrap();
            Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&(
                last["created"].as_i64().unwrap(),
                last["kind"].as_str().unwrap(),
                last["id"].as_str().unwrap(),
            ))?))
        } else {
            None
        };
        Ok(json!({"items":items,"next_cursor":cursor,"page_size":PAGE}))
    }
    pub fn artifact_record(&self, bot: &str, kind: &str, id: &str) -> Result<Value> {
        self.bot(bot)?;
        if kind == "workspace" {let v=self.workspace_artifact_read(id)?;ensure!(v["bot_id"]==bot,"Artifact belongs to another bot");return Ok(v);}
        if kind == "visual" {
            let c=self.0.lock().unwrap();
            let seq:i64=c.query_row("SELECT message_seq FROM visual_panels WHERE id=? AND bot_id=?",params![id,bot],|r|r.get(0))?;
            return crate::visual_panels::record(&c,seq);
        }
        if kind == "snippet" {
            let seq = id.parse::<i64>()?;
            let body:String=self.0.lock().unwrap().query_row("SELECT body FROM chat_messages WHERE seq=? AND sender=? AND kind IN ('result','assistant')",params![seq,bot],|r|r.get(0))?;
            return Ok(json!({"snippets":snippets(&body)}));
        }
        let table = match kind {
            "checklist" => "checklists",
            "reminder" => "reminders",
            _ => anyhow::bail!("Unknown saved record"),
        };
        let c = self.0.lock().unwrap();
        let owner: String = c.query_row(
            &format!("SELECT bot_id FROM {table} WHERE id=?"),
            [id],
            |r| r.get(0),
        )?;
        ensure!(owner == bot, "Artifact does not belong to this bot");
        crate::plans::record(&c, table, id)
    }
}
fn snippets(body: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut fence = None;
    let mut language = String::new();
    let mut source = String::new();
    let mut too_large = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some((marker, count)) = fence {
            if trimmed.chars().count() >= count && trimmed.chars().all(|c| c == marker) {
                if !language.is_empty() {
                    out.push(if too_large {json!({"error":"This shard exceeds the 256 KB rendering limit. Ask the bot to share it as a file."})}else{json!({"language":language,"source":source})});
                }
                fence = None;
                source.clear();
                language.clear();
                too_large = false;
                if out.len() >= 8 {
                    break;
                }
            } else if !language.is_empty() && !too_large {
                if source.len() + line.len() + 1 > 256 * 1024 {
                    too_large = true;
                    source.clear();
                } else {
                    source.push_str(line);
                    source.push('\n');
                }
            }
        } else if let Some(marker) = trimmed.chars().next().filter(|c| matches!(c, '`' | '~')) {
            let count = trimmed.chars().take_while(|c| *c == marker).count();
            if count < 3 {
                continue;
            }
            let tag = trimmed[count..].trim().to_ascii_lowercase();
            language = match tag.as_str() {
                "html" | "shard-html" => "html",
                "jsx" | "react" | "shard-jsx" => "jsx",
                _ => "",
            }
            .into();
            fence = Some((marker, count));
        }
    }
    out
}
pub async fn list(
    State(app): State<Shared>,
    Path(bot): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(app.db.artifact_page(&bot, &query)?))
}
pub async fn record(
    State(app): State<Shared>,
    Path((bot, kind, id)): Path<(String, String, String)>,
) -> Result<Json<Value>, crate::web::Error> {
    Ok(Json(app.db.artifact_record(&bot, &kind, &id)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_pages_metadata_and_loads_one_owned_record() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "codex");
        let other = crate::tests::bot(&app.db, "codex");
        let run = app
            .db
            .run(&app.db.queue(&bot.id, "Artifacts", 0).unwrap())
            .unwrap();
        let list=app.db.checklist_create(&run,json!({"key":"list","title":"Large saved list","items":[{"title":"An item","details":"x".repeat(4000)}]})).unwrap();
        app.db
            .attach_file(
                &run,
                "/workspace/example.html",
                b"<h1>FILE CONTENT NOT IN INDEX</h1>",
            )
            .unwrap();
        app.db.reminder_set(&run,json!({"key":"reminder","message":"Remember this","local_time":"2099-09-10T12:00","timezone":"UTC"})).unwrap();
        // Large history with identical timestamps exercises stable keyset pagination.
        {
            let c = app.db.0.lock().unwrap();
            c.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<5000) INSERT INTO checklists SELECT 'page-'||printf('%05d',x),chat_id,bot_id,run_id,'key-'||x,revision,body,created,updated FROM n,checklists WHERE id=?",[list["id"].as_str().unwrap()]).unwrap();
        }
        let mut before = String::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..251 {
            let page = app
                .db
                .artifact_page(
                    &bot.id,
                    &PageQuery {
                        before: before.clone(),
                        kind: String::new(),
                    },
                )
                .unwrap();
            let items = page["items"].as_array().unwrap();
            assert!(items.len() <= 20);
            assert!(page.to_string().len() < 16000);
            assert!(!page.to_string().contains("FILE CONTENT"));
            assert!(!page.to_string().contains("xxxx"));
            for item in items {
                assert!(seen.insert(item["id"].as_str().unwrap().to_string()));
                assert!(item.get("items").is_none());
            }
            if let Some(next) = page["next_cursor"].as_str() {
                before = next.into();
            } else {
                break;
            }
        }
        assert_eq!(seen.len(), 5003);
        assert_eq!(
            app.db
                .artifact_record(&bot.id, "checklist", list["id"].as_str().unwrap())
                .unwrap()["items"][0]["details"]
                .as_str()
                .unwrap()
                .len(),
            4000
        );
        assert!(
            app.db
                .artifact_record(&other.id, "checklist", list["id"].as_str().unwrap())
                .is_err()
        );
        assert!(
            app.db
                .artifact_page(&other.id, &PageQuery::default())
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            app.db
                .artifact_page(
                    &bot.id,
                    &PageQuery {
                        before: "invalid".into(),
                        kind: String::new()
                    }
                )
                .is_err()
        );
        let files = app
            .db
            .artifact_page(
                &bot.id,
                &PageQuery {
                    before: String::new(),
                    kind: "file".into(),
                },
            )
            .unwrap();
        assert_eq!(files["items"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn inline_artifacts_are_indexed_without_copying_history_or_running_code() {
        let app = crate::tests::app();
        let bot = crate::tests::bot(&app.db, "codex");
        let other = crate::tests::bot(&app.db, "codex");
        let run = app
            .db
            .run(&app.db.queue(&bot.id, "Make a graph", 0).unwrap())
            .unwrap();
        app.db.0.lock().unwrap().execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'result',?,1)",params![run.chat_id,bot.id,"Here is the graph.\n```shard-html\n<h1>Graph</h1>\n```\n~~~jsx\nexport default function App(){return <p>Plot</p>}\n~~~",run.id]).unwrap();
        let page = app
            .db
            .artifact_page(&bot.id, &PageQuery::default())
            .unwrap();
        let item = &page["items"][0];
        assert_eq!(item["kind"], "snippet");
        assert!(!page.to_string().contains("<h1>"));
        let id = item["id"].as_str().unwrap();
        let data = app.db.artifact_record(&bot.id, "snippet", id).unwrap();
        assert_eq!(data["snippets"].as_array().unwrap().len(), 2);
        assert_eq!(data["snippets"][0]["language"], "html");
        assert!(app.db.artifact_record(&other.id, "snippet", id).is_err());
        assert!(snippets("```html\nUnclosed").is_empty());
        assert!(snippets("```html-source\n<p>Code only</p>\n```").is_empty());
        assert_eq!(snippets("```shard-jsx\nexport default function App(){return <p>View</p>}\n```")[0]["language"], "jsx");
        assert!(
            snippets(&format!("```html\n{}\n```", "x".repeat(256 * 1024 + 1)))[0]
                .get("error")
                .is_some()
        );
        app.db
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE chat_messages SET body='Artifact removed' WHERE seq=?",
                [id.parse::<i64>().unwrap()],
            )
            .unwrap();
        assert!(
            app.db
                .artifact_page(&bot.id, &PageQuery::default())
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
