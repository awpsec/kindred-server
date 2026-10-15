//! Durable memory pressure handling. Models compact only the old base; new
//! additions stay verbatim until a later cycle. No background inference or chat.
use crate::db::{self, Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub const PRESSURE_BYTES: usize = 48_000;
pub const COMPACT_BYTES: usize = 40_000;
const PAGE_BYTES: usize = 8_000;

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS memory_versions(bot_id TEXT PRIMARY KEY REFERENCES bots(id) ON DELETE CASCADE,revision INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS memory_overflow(id INTEGER PRIMARY KEY AUTOINCREMENT,bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,source_run TEXT NOT NULL,body TEXT NOT NULL,status TEXT NOT NULL,created INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS memory_overflow_bot ON memory_overflow(bot_id,status,id);
      CREATE TABLE IF NOT EXISTS memory_snapshots(id TEXT PRIMARY KEY,bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,revision INTEGER NOT NULL,body TEXT NOT NULL,overflow_through INTEGER NOT NULL,created INTEGER NOT NULL,committed INTEGER NOT NULL DEFAULT 0,invalidated INTEGER NOT NULL DEFAULT 0,UNIQUE(bot_id,revision));
      CREATE TABLE IF NOT EXISTS memory_snapshot_reads(snapshot_id TEXT NOT NULL REFERENCES memory_snapshots(id) ON DELETE CASCADE,run_id TEXT NOT NULL,through_bytes INTEGER NOT NULL,PRIMARY KEY(snapshot_id,run_id));
      CREATE TRIGGER IF NOT EXISTS memory_version_changed AFTER UPDATE OF memory ON bots WHEN old.memory<>new.memory BEGIN
        INSERT INTO memory_versions VALUES(new.id,1) ON CONFLICT(bot_id) DO UPDATE SET revision=revision+1;
      END;
      CREATE TRIGGER IF NOT EXISTS memory_cleared AFTER UPDATE OF memory ON bots WHEN old.memory<>new.memory AND trim(new.memory)='' BEGIN
        UPDATE memory_overflow SET status='superseded' WHERE bot_id=new.id AND status='pending';
        UPDATE memory_snapshots SET invalidated=1 WHERE bot_id=new.id;
      END;
      CREATE TRIGGER IF NOT EXISTS memory_overflow_added AFTER INSERT ON memory_overflow BEGIN
        INSERT INTO memory_versions VALUES(new.bot_id,1) ON CONFLICT(bot_id) DO UPDATE SET revision=revision+1;
      END;
      CREATE TRIGGER IF NOT EXISTS memory_overflow_changed AFTER UPDATE OF status ON memory_overflow WHEN old.status<>new.status BEGIN
        INSERT INTO memory_versions VALUES(new.bot_id,1) ON CONFLICT(bot_id) DO UPDATE SET revision=revision+1;
      END;
      CREATE TRIGGER IF NOT EXISTS memory_archive_invalidated_insert AFTER INSERT ON continuity_memory_guard WHEN new.invalidated=1 BEGIN
        UPDATE memory_snapshots SET invalidated=1 WHERE bot_id=new.bot_id;
        UPDATE memory_overflow SET status='invalidated' WHERE bot_id=new.bot_id AND status<>'invalidated';
        INSERT INTO memory_versions VALUES(new.bot_id,1) ON CONFLICT(bot_id) DO UPDATE SET revision=revision+1;
      END;
      CREATE TRIGGER IF NOT EXISTS memory_archive_invalidated_update AFTER UPDATE OF invalidated ON continuity_memory_guard WHEN new.invalidated=1 BEGIN
        UPDATE memory_snapshots SET invalidated=1 WHERE bot_id=new.bot_id;
        UPDATE memory_overflow SET status='invalidated' WHERE bot_id=new.bot_id AND status<>'invalidated';
        INSERT INTO memory_versions VALUES(new.bot_id,1) ON CONFLICT(bot_id) DO UPDATE SET revision=revision+1;
      END;")?;
    Ok(())
}

fn invalid(c: &Connection, bot: &str) -> Result<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM continuity_memory_guard WHERE bot_id=? AND invalidated=1)", [bot], |r| r.get(0))?)
}
fn revision(c: &Connection, bot: &str) -> Result<i64> {
    Ok(c.query_row("SELECT revision FROM memory_versions WHERE bot_id=?", [bot], |r| r.get(0)).optional()?.unwrap_or(0))
}
fn current(c: &Connection, bot: &str) -> Result<String> {
    Ok(c.query_row("SELECT memory FROM bots WHERE id=?", [bot], |r| r.get(0))?)
}
fn private(run: &Run) -> bool { run.chat_id == format!("dm-{}", run.bot_id) }
fn valid_session(c: &Connection, run: &Run) -> Result<()> {
    ensure!(!crate::workspace_transfer::frozen(c)?, "Workspace is paused");
    ensure!(crate::continuity::store::session_valid(c, &run.id)?, "Sources changed during this session; rebuild context before saving memory");
    ensure!(c.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=? AND bot_id=? AND chat_id=?)", params![run.id,run.bot_id,run.chat_id], |r| r.get::<_,bool>(0))?, "Memory belongs to this bot and conversation only");
    Ok(())
}
fn private_valid(c: &Connection, run: &Run) -> Result<()> {
    ensure!(private(run), "Private memory is available only in your owner DM");
    valid_session(c, run)?;
    ensure!(!invalid(c, &run.bot_id)?, "Private memory is invalidated; reconstruct from allowed sources before compacting or reading archives");
    Ok(())
}
fn pending(c: &Connection, bot: &str) -> Result<Vec<(i64,String)>> {
    Ok(c.prepare("SELECT id,body FROM memory_overflow WHERE bot_id=? AND status='pending' ORDER BY id")?
        .query_map([bot], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
}
fn append(base: &str, addition: &str) -> String {
    if base.is_empty() { addition.into() } else { format!("{base}\n\n{addition}") }
}
fn has_block(base: &str, addition: &str) -> bool {
    format!("\n\n{base}\n\n").contains(&format!("\n\n{addition}\n\n"))
}
fn stats(c: &Connection, bot: &str) -> Result<(usize,usize)> {
    let (count,bytes):(i64,i64)=c.query_row("SELECT count(*),COALESCE(sum(length(CAST(body AS BLOB))),0) FROM memory_overflow WHERE bot_id=? AND status='pending'", [bot], |r| Ok((r.get(0)?,r.get(1)?)))?;
    Ok((count as usize,bytes as usize))
}

/// Editors display the base, not overflow. Ordinary edits must preserve those
/// unseen accepted additions. An explicit clear must never resurrect old facts.
pub fn editor_replaced(c: &Connection, bot: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        c.execute("UPDATE memory_overflow SET status='superseded' WHERE bot_id=? AND status='pending'", [bot])?;
        c.execute("UPDATE memory_snapshots SET invalidated=1 WHERE bot_id=?", [bot])?;
    }
    // Even an explicit same-text save is a new intent, invalidating old snapshots.
    c.execute("INSERT INTO memory_versions VALUES(?,1) ON CONFLICT(bot_id) DO UPDATE SET revision=revision+1", [bot])?;
    Ok(())
}

pub fn remember(db: &Db, run: &Run, args: &Value) -> Result<Value> {
    let proposed=args["text"].as_str().ok_or_else(||anyhow::anyhow!("Missing memory text"))?;
    ensure!(proposed.len()<=db::BOT_MEMORY_MAX_BYTES,"Each memory save is limited to 64,000 UTF-8 bytes; split larger additions into meaningful facts");
    let mode=args.get("mode").map(|v|v.as_str().unwrap_or("")).unwrap_or("legacy");
    ensure!(matches!(mode,"legacy"|"append"|"replace"),"Memory mode must be append or replace");
    ensure!(mode!="replace" || private(run),"Memory replacement requires your owner DM. Use mode=append here");
    let mut c=db.0.lock().unwrap();
    let tx=c.transaction()?;
    valid_session(&tx,run)?;
    let old=current(&tx,&run.bot_id)?;
    let replacement=mode!="append" && private(run);
    ensure!(!invalid(&tx,&run.bot_id)? || replacement,"Private memory needs reconstruction in the owner DM; save current shared facts with continuity_save");
    if replacement {
        editor_replaced(&tx,&run.bot_id,proposed)?;
        tx.execute("UPDATE bots SET memory=? WHERE id=?",params![proposed,run.bot_id])?;
    } else {
        let addition=proposed;
        ensure!(!addition.trim().is_empty(),"Memory addition must not be empty");
        // Legacy shared callers sometimes supplied the old text followed by a
        // new fact. Retain the old merge behavior without losing pending facts.
        let merged=if mode=="legacy" && (old.is_empty() || proposed.contains(&old)) { proposed.to_owned() }
            else { append(&old,addition) };
        let duplicate=has_block(&old,addition) || tx.query_row("SELECT EXISTS(SELECT 1 FROM memory_overflow WHERE bot_id=? AND status='pending' AND body=?)",params![run.bot_id,addition],|r|r.get::<_,bool>(0))?;
        if !duplicate {
            if merged.len()<=db::BOT_MEMORY_MAX_BYTES && stats(&tx,&run.bot_id)?.0==0 {
                tx.execute("UPDATE bots SET memory=? WHERE id=?",params![merged,run.bot_id])?;
            } else {
                tx.execute("INSERT INTO memory_overflow(bot_id,source_run,body,status,created) VALUES(?,?,?,'pending',?)",params![run.bot_id,run.id,addition,db::now()])?;
            }
        }
    }
    let bytes=current(&tx,&run.bot_id)?.len();
    let (count,pending_bytes)=stats(&tx,&run.bot_id)?;
    tx.commit()?;
    Ok(json!({"text":if count>0 {"Memory saved locally. The new facts are retained verbatim in durable pending memory. Compact the existing base in your owner DM before folding them in; do not ask the owner to repeat them."} else if bytes>=PRESSURE_BYTES {"Memory saved locally. Memory is nearing capacity; compact the existing base in your owner DM now."} else {"Memory saved locally."},"saved":true,"memory_bytes":bytes,"pending_count":count,"pending_bytes":pending_bytes,"compaction_needed":bytes>=PRESSURE_BYTES || count>0}))
}

pub fn context(db: &Db, run: &Run) -> Result<Value> {
    if !private(run) { return Ok(Value::Null); }
    let c=db.0.lock().unwrap();
    if invalid(&c,&run.bot_id)? || !crate::continuity::store::session_valid(&c,&run.id)? { return Ok(json!({"invalidated":true})); }
    let memory=current(&c,&run.bot_id)?;
    let (count,bytes)=stats(&c,&run.bot_id)?;
    if memory.len()<PRESSURE_BYTES && count==0 {
        return Ok(json!({"due":false,"memory_bytes":memory.len(),"pending_count":0}));
    }
    let mut remaining=PAGE_BYTES;
    let mut additions=Vec::new();
    for (id,body) in pending(&c,&run.bot_id)? {
        if remaining==0 { break; }
        let excerpt=crate::runtime::bounded(&body,remaining);
        remaining-=excerpt.len();
        additions.push(json!({"id":id,"text":excerpt,"shortened":excerpt.len()<body.len()}));
    }
    Ok(json!({"due":memory.len()>=PRESSURE_BYTES || count>0,"memory_bytes":memory.len(),"pending_count":count,"pending_bytes":bytes,"pending_additions":additions,"pending_shortened":bytes>PAGE_BYTES,"read_pending_offset":memory.len(),"compaction_needed":memory.len()>=PRESSURE_BYTES || count>0,"target_bytes":COMPACT_BYTES,"instructions":"At memory pressure, compact the old base quietly during this private turn before proceeding. Call memory_compact with no arguments, read every snapshot page with memory_read(snapshot_id,offset), then submit the snapshot_id and compacted base text. Preserve standing rules, preferences, prohibitions, responsibilities, attribution and newer corrections. Do not include pending additions in the submitted text; Kindred appends them verbatim. The original remains recoverable. Never alter work rules or instructions as compaction. Pending facts are already saved; read missing pending text with memory_read and do not request repetition."}))
}

fn page(text: &str, offset: usize) -> Result<Value> {
    ensure!(offset<=text.len() && text.is_char_boundary(offset),"Invalid memory offset");
    let part=crate::runtime::bounded(&text[offset..],PAGE_BYTES);
    Ok(json!({"memory":part,"offset":offset,"next_offset":if offset+part.len()<text.len(){Some(offset+part.len())}else{None},"total_bytes":text.len()}))
}
fn offset(args: &Value) -> Result<usize> {
    match args.get("offset") {
        None => Ok(0),
        Some(v) => usize::try_from(v.as_u64().ok_or_else(||anyhow::anyhow!("Invalid memory offset"))?).map_err(Into::into),
    }
}
pub fn read(db: &Db, run: &Run, args: &Value) -> Result<Value> {
    let c=db.0.lock().unwrap();
    private_valid(&c,run)?;
    let snapshot_id=args["snapshot_id"].as_str();
    let mut text=if let Some(id)=snapshot_id {
        c.query_row("SELECT body FROM memory_snapshots WHERE id=? AND bot_id=? AND invalidated=0",params![id,run.bot_id],|r|r.get::<_,String>(0)).optional()?.ok_or_else(||anyhow::anyhow!("Memory snapshot is unavailable or invalidated"))?
    } else { current(&c,&run.bot_id)? };
    if snapshot_id.is_none() { for (_,body) in pending(&c,&run.bot_id)? {text=append(&text,&body);} }
    let mut out=page(&text,offset(args)?)?;
    if let Some(id)=snapshot_id {
        let start=offset(args)?;
        let end=start+out["memory"].as_str().unwrap().len();
        c.execute("INSERT INTO memory_snapshot_reads VALUES(?,?,?) ON CONFLICT(snapshot_id,run_id) DO UPDATE SET through_bytes=max(through_bytes,excluded.through_bytes) WHERE through_bytes>=?",params![id,run.id,if start==0{end}else{0},start])?;
        // Sequential or overlapping reads extend coverage; skipping a page does not.
        c.execute("UPDATE memory_snapshot_reads SET through_bytes=max(through_bytes,?) WHERE snapshot_id=? AND run_id=? AND through_bytes>=?",params![end,id,run.id,start])?;
    }
    out["snapshot_id"]=json!(snapshot_id);
    out["includes_pending"]=json!(snapshot_id.is_none());
    Ok(out)
}
pub fn snapshot(db: &Db, run: &Run) -> Result<Value> {
    let mut c=db.0.lock().unwrap();
    let tx=c.transaction()?;
    private_valid(&tx,run)?;
    let rev=revision(&tx,&run.bot_id)?;
    let memory=current(&tx,&run.bot_id)?;
    let existing:Option<String>=tx.query_row("SELECT id FROM memory_snapshots WHERE bot_id=? AND revision=? AND invalidated=0",params![run.bot_id,rev],|r|r.get(0)).optional()?;
    let id=if let Some(id)=existing {id} else {
        let id=db::id();
        let through:i64=tx.query_row("SELECT COALESCE(max(id),0) FROM memory_overflow WHERE bot_id=? AND status='pending'",[&run.bot_id],|r|r.get(0))?;
        tx.execute("INSERT INTO memory_snapshots(id,bot_id,revision,body,overflow_through,created) VALUES(?,?,?,?,?,?)",params![id,run.bot_id,rev,memory,through,db::now()])?;
        id
    };
    let (count,bytes)=stats(&tx,&run.bot_id)?;
    let mut out=page(&memory,0)?;
    tx.execute("INSERT INTO memory_snapshot_reads VALUES(?,?,?) ON CONFLICT(snapshot_id,run_id) DO UPDATE SET through_bytes=max(through_bytes,excluded.through_bytes)",params![id,run.id,out["memory"].as_str().unwrap().len()])?;
    out["snapshot_id"]=json!(id);
    out["pending_count"]=json!(count);
    out["pending_bytes"]=json!(bytes);
    out["target_bytes"]=json!(COMPACT_BYTES);
    out["instructions"]=json!("Read all remaining snapshot pages with memory_read using this snapshot_id. Submit a compacted version of ONLY this base memory with memory_compact(snapshot_id,text). Preserve rules, preferences, responsibilities, corrections and attribution. Do not copy pending additions; they are appended verbatim by the server. Originals are retained for recovery.");
    tx.commit()?;
    Ok(out)
}
pub fn compact(db: &Db, run: &Run, args: &Value) -> Result<Value> {
    let id=args["snapshot_id"].as_str().ok_or_else(||anyhow::anyhow!("Prepare a memory snapshot first"))?;
    let proposed=args["text"].as_str().ok_or_else(||anyhow::anyhow!("Supply compacted base memory"))?.trim();
    ensure!(!proposed.is_empty() && proposed.len()<=COMPACT_BYTES,"Compacted base memory must contain 1–40,000 UTF-8 bytes");
    let mut c=db.0.lock().unwrap();
    let tx=c.transaction()?;
    private_valid(&tx,run)?;
    let (rev,old,through,committed):(i64,String,i64,bool)=tx.query_row("SELECT revision,body,overflow_through,committed FROM memory_snapshots WHERE id=? AND bot_id=? AND invalidated=0",params![id,run.bot_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or_else(||anyhow::anyhow!("Memory snapshot is unavailable or invalidated"))?;
    ensure!(!committed,"This memory snapshot was already compacted; read the current memory before making another change");
    let read_through:i64=tx.query_row("SELECT through_bytes FROM memory_snapshot_reads WHERE snapshot_id=? AND run_id=?",params![id,run.id],|r|r.get(0)).optional()?.unwrap_or(0);
    ensure!(read_through as usize==old.len(),"Read every snapshot page before compacting; unread memory must not be discarded");
    ensure!(revision(&tx,&run.bot_id)?==rev && current(&tx,&run.bot_id)?==old,"Memory changed after the snapshot; prepare a new snapshot and preserve the latest facts");
    ensure!(old.is_empty() || proposed.len()<old.len(),"Compaction must reduce the existing base; keep new facts in pending memory");
    let mut merged=proposed.to_owned();
    let mut folded=Vec::new();
    for (pending_id,body) in pending(&tx,&run.bot_id)? {
        if pending_id>through {break;}
        let next=append(&merged,&body);
        if next.len()>db::BOT_MEMORY_MAX_BYTES {break;}
        merged=next;
        folded.push(pending_id);
    }
    // Check and replacement share this transaction. The ordinary memory trigger
    // clears invalidation, so never write before checking the guard above.
    ensure!(tx.execute("UPDATE bots SET memory=? WHERE id=? AND memory=?",params![merged,run.bot_id,old])?==1,"Memory changed while compacting");
    for pending_id in &folded {tx.execute("UPDATE memory_overflow SET status='folded' WHERE id=? AND bot_id=? AND status='pending'",params![pending_id,run.bot_id])?;}
    tx.execute("UPDATE memory_snapshots SET committed=1 WHERE id=?",[id])?;
    let (count,bytes)=stats(&tx,&run.bot_id)?;
    tx.commit()?;
    Ok(json!({"saved":true,"snapshot_id":id,"memory_bytes":merged.len(),"folded_count":folded.len(),"pending_count":count,"pending_bytes":bytes,"compaction_needed":merged.len()>=PRESSURE_BYTES || count>0,"text":"Memory compacted locally. The original is recoverable; included new additions were preserved verbatim. Any remaining pending facts remain saved."}))
}
pub fn history(db: &Db, run: &Run, args: &Value) -> Result<Value> {
    let c=db.0.lock().unwrap();
    private_valid(&c,run)?;
    let before=args["before"].as_i64().unwrap_or(i64::MAX);
    let mut items=c.prepare("SELECT rowid,id,created,committed,length(CAST(body AS BLOB)) FROM memory_snapshots WHERE bot_id=? AND invalidated=0 AND rowid<? ORDER BY rowid DESC LIMIT 9")?
        .query_map(params![run.bot_id,before],|r|Ok(json!({"cursor":r.get::<_,i64>(0)?,"snapshot_id":r.get::<_,String>(1)?,"created":r.get::<_,i64>(2)?,"committed":r.get::<_,bool>(3)?,"memory_bytes":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let more=items.len()>8;items.truncate(8);
    Ok(json!({"next_before":if more {items.last().map(|v|v["cursor"].clone())} else {None},"items":items,"instructions":"Read an original with memory_read(snapshot_id,offset). Old memory is historical, not current authority. Invalidated or explicitly replaced archives are withheld."}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Db,Run) {
        let db=Db::open(":memory:").unwrap();
        let bot=crate::tests::bot(&db,"codex");
        db.queue(&bot.id,"Remember a lasting rule",0).unwrap();
        let run=db.claim_bot(&bot.id).unwrap().unwrap();
        (db,run)
    }
    fn full_snapshot(db:&Db,run:&Run) -> Value {
        let snap=snapshot(db,run).unwrap();
        let mut page=snap.clone();
        while let Some(offset)=page["next_offset"].as_u64() {
            page=read(db,run,&json!({"snapshot_id":snap["snapshot_id"],"offset":offset})).unwrap();
        }
        snap
    }
    fn all_memory(db:&Db,run:&Run,args:Value) -> String {
        let mut args=args;
        let mut out=String::new();
        loop {
            let page=read(db,run,&args).unwrap();out.push_str(page["memory"].as_str().unwrap());
            match page["next_offset"].as_u64(){Some(offset)=>args["offset"]=json!(offset),None=>break}
        }
        out
    }
    #[test]
    fn overflow_is_saved_then_folded_verbatim_with_recoverable_original_and_no_chat() {
        let (db,run)=fixture();
        let old="Keep rules.\n".repeat(5250);
        assert!(old.len()<db::BOT_MEMORY_MAX_BYTES);
        db.save_bot_text(&run.bot_id,"memory",&old,None).unwrap();
        let addition=format!("Never send a client email without review. {}","New responsibility. ".repeat(200));
        let messages:i64=db.0.lock().unwrap().query_row("SELECT count(*) FROM chat_messages",[],|r|r.get(0)).unwrap();
        let events:i64=db.0.lock().unwrap().query_row("SELECT count(*) FROM events",[],|r|r.get(0)).unwrap();
        let receipt=remember(&db,&run,&json!({"mode":"append","text":addition})).unwrap();
        assert_eq!(receipt["saved"],true);assert_eq!(receipt["pending_count"],1);
        assert_eq!(db.bot(&run.bot_id).unwrap().memory,old);
        assert!(all_memory(&db,&run,json!({})).ends_with(&addition));
        assert_eq!(context(&db,&run).unwrap()["due"],true);
        let snap=full_snapshot(&db,&run);
        assert_eq!(snapshot(&db,&run).unwrap()["snapshot_id"],snap["snapshot_id"]);
        let summary="Keep the owner's rules.";
        let result=compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":summary})).unwrap();
        assert_eq!(result["folded_count"],1);
        assert_eq!(db.bot(&run.bot_id).unwrap().memory,append(summary,&addition));
        assert_eq!(all_memory(&db,&run,json!({"snapshot_id":snap["snapshot_id"]})),old);
        assert_eq!(history(&db,&run,&json!({})).unwrap()["items"][0]["committed"],true);
        assert_eq!(db.0.lock().unwrap().query_row("SELECT count(*) FROM chat_messages",[],|r|r.get::<_,i64>(0)).unwrap(),messages);
        assert_eq!(db.0.lock().unwrap().query_row("SELECT count(*) FROM events",[],|r|r.get::<_,i64>(0)).unwrap(),events);
        assert!(compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":"Again"})).is_err());
    }
    #[test]
    fn unread_pages_and_concurrent_writes_reject_without_losing_pending_facts() {
        let (db,run)=fixture();
        db.save_bot_text(&run.bot_id,"memory",&"é".repeat(31000),None).unwrap();
        let snap=snapshot(&db,&run).unwrap();
        assert!(compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":"Compact"})).is_err());
        read(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"offset":56000})).unwrap();
        assert!(compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":"Compact"})).is_err());
        assert!(read(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"offset":1})).is_err());
        let snap=full_snapshot(&db,&run);
        let addition="New permanent rule. ".repeat(300);
        remember(&db,&run,&json!({"mode":"append","text":addition})).unwrap();
        assert!(compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":"Compact"})).is_err());
        assert!(all_memory(&db,&run,json!({})).ends_with(&addition));
        let snap=full_snapshot(&db,&run);
        db.save_bot_text(&run.bot_id,"memory","Owner edited the visible base",None).unwrap();
        assert!(compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":"Compact"})).is_err());
        assert!(all_memory(&db,&run,json!({})).ends_with(&addition));
        // The editor cannot silently discard unseen pending facts; clear is explicit.
        // Cover non-editor writers too (sync or older clients can update bots).
        db.0.lock().unwrap().execute("UPDATE bots SET memory='' WHERE id=?",[&run.bot_id]).unwrap();
        assert_eq!(all_memory(&db,&run,json!({})),"");
        assert!(read(&db,&run,&json!({"snapshot_id":snap["snapshot_id"]})).is_err());
        assert_eq!(context(&db,&run).unwrap()["pending_count"],0);
    }
    #[test]
    fn source_invalidation_quarantines_archives_even_after_reconstruction() {
        let (db,run)=fixture();
        db.save_bot_text(&run.bot_id,"memory",&"private fact ".repeat(4800),None).unwrap();
        let snap=full_snapshot(&db,&run);
        remember(&db,&run,&json!({"mode":"append","text":"new fact ".repeat(1000)})).unwrap();
        db.0.lock().unwrap().execute("INSERT INTO continuity_memory_guard VALUES(?,1)",[&run.bot_id]).unwrap();
        assert!(compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":"Compact"})).is_err());
        assert!(read(&db,&run,&json!({})).is_err());
        remember(&db,&run,&json!({"mode":"replace","text":"Rebuilt from retained sources"})).unwrap();
        assert_eq!(all_memory(&db,&run,json!({})),"Rebuilt from retained sources");
        assert!(read(&db,&run,&json!({"snapshot_id":snap["snapshot_id"]})).is_err());
        assert!(history(&db,&run,&json!({})).unwrap()["items"].as_array().unwrap().is_empty());
    }
    #[test]
    fn shared_rooms_cannot_read_private_memory_but_can_save_without_disclosure() {
        let (db,run)=fixture();
        db.save_bot_text(&run.bot_id,"memory",&"secret private preference ".repeat(2400),None).unwrap();
        let snap=full_snapshot(&db,&run);
        let mut shared=run.clone();shared.chat_id="room".into();
        db.0.lock().unwrap().execute("UPDATE runs SET chat_id=? WHERE id=?",params![shared.chat_id,shared.id]).unwrap();
        assert!(read(&db,&shared,&json!({})).is_err());
        assert!(snapshot(&db,&shared).is_err());
        assert!(history(&db,&shared,&json!({})).is_err());
        assert!(compact(&db,&shared,&json!({"snapshot_id":snap["snapshot_id"],"text":"Compact"})).is_err());
        assert!(context(&db,&shared).unwrap().is_null());
        let receipt=remember(&db,&shared,&json!({"mode":"append","text":"New public assignment ".repeat(100)})).unwrap();
        assert_eq!(receipt["saved"],true);
        assert!(!receipt.to_string().contains("secret private preference"));
        assert!(remember(&db,&shared,&json!({"mode":"replace","text":"Replace"})).is_err());
    }
    #[test]
    fn failed_or_oversized_compaction_does_not_destroy_saved_additions() {
        let (db,run)=fixture();
        db.save_bot_text(&run.bot_id,"memory",&"x".repeat(63000),None).unwrap();
        let receipt=remember(&db,&run,&json!({"mode":"append","text":"y".repeat(64000)})).unwrap();
        assert_eq!(receipt["pending_count"],1);
        let snap=full_snapshot(&db,&run);
        for text in ["".into(),"z".repeat(COMPACT_BYTES+1)] {
            assert!(compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":text})).is_err());
        }
        let result=compact(&db,&run,&json!({"snapshot_id":snap["snapshot_id"],"text":"Retained base"})).unwrap();
        assert_eq!(result["pending_count"],1);
        assert!(all_memory(&db,&run,json!({})).ends_with(&"y".repeat(64000)));
    }
    #[test]
    fn snapshot_and_pending_survive_restart_but_page_coverage_must_be_reestablished_for_new_run() {
        let path=std::env::temp_dir().join(format!("kindred-memory-maintenance-{}.db",db::id()));
        let db=Db::open(path.to_str().unwrap()).unwrap();
        let bot=crate::tests::bot(&db,"codex");
        db.queue(&bot.id,"Remember",0).unwrap();let run=db.claim_bot(&bot.id).unwrap().unwrap();
        db.save_bot_text(&run.bot_id,"memory",&"a".repeat(63000),None).unwrap();
        remember(&db,&run,&json!({"mode":"append","text":"b".repeat(2000)})).unwrap();
        let snap=full_snapshot(&db,&run);drop(db);
        let reopened=Db::open(path.to_str().unwrap()).unwrap();
        assert_eq!(context(&reopened,&run).unwrap()["pending_count"],1);
        assert_eq!(snapshot(&reopened,&run).unwrap()["snapshot_id"],snap["snapshot_id"]);
        reopened.queue(&bot.id,"Continue",0).unwrap();let next=reopened.claim_bot(&bot.id).unwrap().unwrap();
        assert!(compact(&reopened,&next,&json!({"snapshot_id":snap["snapshot_id"],"text":"Retained"})).is_err());
        full_snapshot(&reopened,&next);
        assert_eq!(compact(&reopened,&next,&json!({"snapshot_id":snap["snapshot_id"],"text":"Retained"})).unwrap()["folded_count"],1);
        drop(reopened);
        for suffix in ["",".lock","-wal","-shm"] {let _=std::fs::remove_file(format!("{}{suffix}",path.display()));}
    }
    #[test]
    fn transfer_restores_pending_and_archives_without_import_trigger_collisions() {
        for invalidated in [false,true] {
            let (db,run)=fixture();
            db.save_bot_text(&run.bot_id,"memory",&"a".repeat(63000),None).unwrap();
            remember(&db,&run,&json!({"mode":"append","text":"b".repeat(2000)})).unwrap();
            let snap=full_snapshot(&db,&run);
            if invalidated { db.0.lock().unwrap().execute("INSERT INTO continuity_memory_guard VALUES(?,1)",[&run.bot_id]).unwrap(); }
            db.finish(&run.id,"completed","","").unwrap();
            let rev=revision(&db.0.lock().unwrap(),&run.bot_id).unwrap();
            let package=db.prepare_transfer(&db::id(),"Fixture").unwrap();
            let destination=Db::open(":memory:").unwrap();
            destination.import_transfer(&package).unwrap();
            assert_eq!(revision(&destination.0.lock().unwrap(),&run.bot_id).unwrap(),rev);
            if invalidated {
                assert!(read(&destination,&run,&json!({"snapshot_id":snap["snapshot_id"]})).is_err());
                destination.save_bot_text(&run.bot_id,"memory","Rebuilt",None).unwrap();
                assert!(read(&destination,&run,&json!({"snapshot_id":snap["snapshot_id"]})).is_err());
                assert_eq!(all_memory(&destination,&run,json!({})),"Rebuilt");
            } else {
                assert_eq!(context(&destination,&run).unwrap()["pending_count"],1);
                assert_eq!(all_memory(&destination,&run,json!({"snapshot_id":snap["snapshot_id"]})),"a".repeat(63000));
            }
        }
    }
    #[test]
    fn deleting_a_bot_removes_its_private_memory_records() {
        let db=Db::open(":memory:").unwrap();
        let bot=crate::tests::bot(&db,"codex");
        db.save_bot_text(&bot.id,"memory","Private preference",None).unwrap();
        let c=db.0.lock().unwrap();
        c.execute("INSERT INTO memory_overflow(bot_id,source_run,body,status,created) VALUES(?,'','Private addition','pending',0)",[&bot.id]).unwrap();
        c.execute("INSERT INTO memory_snapshots(id,bot_id,revision,body,overflow_through,created) VALUES('fixture',?,0,'Original',0,0)",[&bot.id]).unwrap();
        c.execute("INSERT INTO memory_snapshot_reads VALUES('fixture','fixture',8)",[]).unwrap();
        c.execute("DELETE FROM bots WHERE id=?",[&bot.id]).unwrap();
        for table in ["memory_versions","memory_overflow","memory_snapshots","memory_snapshot_reads"] {
            assert_eq!(c.query_row(&format!("SELECT count(*) FROM {table}"),[],|r|r.get::<_,i64>(0)).unwrap(),0);
        }
    }
}
