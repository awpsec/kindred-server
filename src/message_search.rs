//! Search only visible conversation text. Never index provider transcripts,
//! memory, connector payloads, or another profile's database.
use anyhow::{Result, ensure};
use axum::{Json, extract::{Query, State}};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const SCAN_LIMIT: usize = 20_000;
const TEXT_LIMIT: usize = 16_384;
const RESULT_LIMIT: usize = 24;
const BYTE_BUDGET: usize = 4 * 1024 * 1024;
#[derive(Deserialize)]
pub(crate) struct Search { pub q: String }

pub(crate) fn terms(query: &str) -> Result<Vec<String>> {
    let query = query.trim();
    ensure!((2..=160).contains(&query.chars().count()), "Enter between 2 and 160 characters");
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    ensure!(terms.len() <= 12, "Use at most 12 search terms");
    Ok(terms)
}

// Unicode-safe excerpts around the first match, with no rendered HTML.
fn excerpt(body: &str, terms: &[String]) -> String {
    let chars: Vec<char> = body.chars().collect();
    let lowered = body.to_lowercase();
    let at = terms.iter().filter_map(|term| lowered.find(term)).min().unwrap_or(0);
    // Unicode lowercasing can expand one character (for example İ). Map the
    // folded byte position back to its original character before excerpting.
    let mut folded_end=0;
    let index=chars.iter().position(|ch| {
        folded_end+=ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        folded_end>at
    }).unwrap_or(0);
    let start = index.saturating_sub(65);
    let end = (start + 240).min(chars.len());
    format!("{}{}{}", if start > 0 { "…" } else { "" }, chars[start..end].iter().collect::<String>(), if end < chars.len() { "…" } else { "" })
}

pub(crate) fn collect(c: &Connection, sql: &str, scope: &str, terms: &[String]) -> Result<Value> {
    let mut statement = c.prepare(sql)?;
    let mut cursor = statement.query(params![scope, SCAN_LIMIT + 1, TEXT_LIMIT])?;
    let mut matches = Vec::new();
    let mut examined = 0;
    let mut bytes = 0;
    let mut limited = false;
    loop {
        // Reserve a complete UTF-8 row before fetching it; even the final row
        // stays inside the aggregate budget.
        if examined == SCAN_LIMIT || bytes > BYTE_BUDGET - TEXT_LIMIT * 4 { limited = true; break; }
        let Some(row) = cursor.next()? else { break; };
        examined += 1;
        let body: String = row.get(3)?;
        bytes += body.len();
        if body.chars().count() >= TEXT_LIMIT { limited = true; }
        let lowered = body.to_lowercase();
        if !terms.iter().all(|term| lowered.contains(term)) { continue; }
        if matches.len() == RESULT_LIMIT { limited = true; break; }
        matches.push(json!({"chat_id":row.get::<_,String>(0)?,"seq":row.get::<_,i64>(1)?,"sender":row.get::<_,String>(2)?,"excerpt":excerpt(&body,terms),"created":row.get::<_,i64>(4)?}));
    }
    Ok(json!({"matches":matches,"limited":limited,"scope":"recent_messages","scan_limit":SCAN_LIMIT,"text_limit":TEXT_LIMIT,"byte_budget":BYTE_BUDGET,"bytes_examined":bytes}))
}

pub(crate) fn has_older(c: &Connection, shared: bool) -> Result<bool> {
    let sql=if shared {
        "SELECT EXISTS(SELECT 1 FROM server_messages WHERE seq<=(SELECT COALESCE(MAX(seq),0)-?1 FROM server_messages))"
    } else {
        "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE seq<=(SELECT COALESCE(MAX(seq),0)-?1 FROM chat_messages))"
    };
    Ok(c.query_row(sql,[SCAN_LIMIT+1],|row|row.get(0))?)
}

pub(crate) fn local(c: &Connection, query: &str) -> Result<Value> {
    let terms = terms(query)?;
    // Profile authentication selects this Db before the handler runs. Server
    // chat mirrors are excluded; shared messages have their own membership gate.
    let mut result=collect(c, "SELECT m.chat_id,m.seq,m.sender,substr(m.body,1,?3),m.created FROM chat_messages m JOIN chats c ON c.id=m.chat_id WHERE ?1='' AND m.seq>(SELECT COALESCE(MAX(seq),0)-?2 FROM chat_messages) AND c.archived=0 AND c.id NOT LIKE 'server-%' AND (c.id NOT LIKE 'dm-%' OR EXISTS(SELECT 1 FROM bots b WHERE c.id='dm-'||b.id AND COALESCE(json_extract(b.profile,'$.archived'),0)=0)) AND m.suppressed=0 AND m.kind IN ('message','assistant','result') ORDER BY m.seq DESC LIMIT ?2", "", &terms)?;
    if has_older(c,false)? { result["limited"]=json!(true); }
    Ok(result)
}

pub(crate) async fn route(State(app): State<crate::runtime::Shared>, Query(query): Query<Search>) -> Result<Json<Value>, crate::web::Error> {
    terms(&query.q)?;
    Ok(Json(tokio::task::spawn_blocking(move || local(&app.db.0.lock().unwrap(), &query.q)).await??))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{app, bot};
    fn insert(c: &Connection, chat: &str, text: &str, kind: &str, suppressed: bool) {
        c.execute("INSERT OR IGNORE INTO chats(id,name,members) VALUES(?,'Search fixture','[]')",[chat]).unwrap();
        c.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,created,suppressed) VALUES(?,'user',?,?,1,?)", params![chat,text,kind,suppressed]).unwrap();
    }
    #[test]
    fn visible_text_only_and_literal_queries() {
        let app=app(); let b=bot(&app.db,"codex");let chat=format!("dm-{}",b.id);
        let c=app.db.0.lock().unwrap();
        insert(&c,&chat,"Kickoff confirmed Friday 100%_literal", "message",false);
        insert(&c,&chat,"Kickoff suppressed", "message",true);
        insert(&c,&chat,"Kickoff private connector payload", "connector_artifact",false);
        let found=local(&c,"kickoff FRIDAY").unwrap();assert_eq!(found["matches"].as_array().unwrap().len(),1);
        assert_eq!(local(&c,"%_literal").unwrap()["matches"].as_array().unwrap().len(),1);
        assert!(local(&c,"suppressed").unwrap()["matches"].as_array().unwrap().is_empty());
        assert!(local(&c,"connector payload").unwrap()["matches"].as_array().unwrap().is_empty());
        assert!(local(&c,"x").is_err());assert!(local(&c,&"x".repeat(161)).is_err());
    }
    #[test]
    fn archived_and_other_profiles_are_not_searched() {
        let app=app();let other=crate::tests::app();let b=bot(&app.db,"codex");let o=bot(&other.db,"codex");
        
        insert(&other.db.0.lock().unwrap(),&format!("dm-{}",o.id),"private sentinel", "message",false);
        let c=app.db.0.lock().unwrap();insert(&c,&format!("dm-{}",b.id),"own sentinel","message",false);
        assert!(local(&c,"private sentinel").unwrap()["matches"].as_array().unwrap().is_empty());
        c.execute("UPDATE chats SET archived=1 WHERE id=?",[format!("dm-{}",b.id)]).unwrap();
        assert!(local(&c,"own sentinel").unwrap()["matches"].as_array().unwrap().is_empty());
    }
    #[test]
    fn raw_sequence_budget_excludes_old_matches_and_reports_partial() {
        let app=app();let b=bot(&app.db,"codex");let chat=format!("dm-{}",b.id);let c=app.db.0.lock().unwrap();
        insert(&c,&chat,"older kickoff","message",false);
        c.execute("INSERT INTO chat_messages(seq,chat_id,sender,body,kind,created,suppressed) VALUES(?1,?2,'user','hidden','message',1,1)",params![SCAN_LIMIT+10,chat]).unwrap();
        let result=local(&c,"kickoff").unwrap();assert_eq!(result["limited"],true);assert!(result["matches"].as_array().unwrap().is_empty());
    }
    #[test]
    fn oversized_messages_stop_at_aggregate_budget() {
        let app=app();let b=bot(&app.db,"codex");let c=app.db.0.lock().unwrap();
        for _ in 0..300 {insert(&c,&format!("dm-{}",b.id),&"中".repeat(TEXT_LIMIT),"message",false);}
        let result=local(&c,"absent").unwrap();assert_eq!(result["limited"],true);
        assert!(result["bytes_examined"].as_u64().unwrap()<=BYTE_BUDGET as u64);
        assert!(result["matches"].as_array().unwrap().is_empty());
    }
    #[test]
    fn unicode_excerpts_and_result_limit() {
        assert!(excerpt(&format!("{} marker {}", "İ".repeat(300), "中".repeat(500)), &["marker".into()]).contains("marker"));
        let app=app();let b=bot(&app.db,"codex");let c=app.db.0.lock().unwrap();
        for _ in 0..30 {insert(&c,&format!("dm-{}",b.id),&format!("{} ΓΕΙΑ kickoff {}", "🙂".repeat(90),"中".repeat(300)),"message",false);}
        let result=local(&c,"γεια").unwrap();assert_eq!(result["matches"].as_array().unwrap().len(),24);assert_eq!(result["limited"],true);
        assert!(result["matches"][0]["excerpt"].as_str().unwrap().contains("ΓΕΙΑ"));assert!(result["matches"][0]["excerpt"].as_str().unwrap().chars().count()<=242);
    }
}
