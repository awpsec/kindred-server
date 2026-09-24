//! Persistent, data-only workflow panels. Rendering never executes a purchase or trade.
use crate::db::{self, Db, Run};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS visual_panels(id TEXT PRIMARY KEY,bot_id TEXT NOT NULL REFERENCES bots(id),chat_id TEXT NOT NULL REFERENCES chats(id),panel_key TEXT NOT NULL,body TEXT NOT NULL,revision INTEGER NOT NULL,run_id TEXT NOT NULL REFERENCES runs(id),message_seq INTEGER NOT NULL UNIQUE REFERENCES chat_messages(seq),UNIQUE(bot_id,chat_id,panel_key));")?;
    crate::workflow_panels::migrate(c)?;
    Ok(())
}
fn text(v: &Value, key: &str, max: usize) -> Result<()> {
    let s = v[key].as_str().unwrap_or("");
    ensure!(
        !s.trim().is_empty() && s.len() <= max,
        "{key} must contain 1–{max} bytes"
    );
    Ok(())
}
fn url(v: &Value, key: &str, required: bool) -> Result<()> {
    if v[key].is_null() && !required {
        return Ok(());
    }
    let value = v[key].as_str().unwrap_or("");
    let u = reqwest::Url::parse(value)?;
    ensure!(
        value.len() <= 2000
            && u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none(),
        "{key} must be an HTTPS URL without credentials"
    );
    Ok(())
}
fn number(v: &Value, key: &str, required: bool) -> Result<()> {
    if v[key].is_null() && !required {
        return Ok(());
    }
    ensure!(
        v[key]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.abs() <= 1e18),
        "{key} must be a finite number"
    );
    Ok(())
}
fn currency(v: &Value) -> Result<()> {
    let c = v["currency"].as_str().unwrap_or("");
    ensure!(
        c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()),
        "Use a three-letter currency code"
    );
    Ok(())
}
fn points(v: &Value, min: usize) -> Result<()> {
    let points = v["points"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("points must be an array"))?;
    ensure!(
        (min..=500).contains(&points.len()),
        "Use {min}–500 points per series"
    );
    let mut last = f64::NEG_INFINITY;
    for p in points {
        number(p, "x", true)?;
        number(p, "y", true)?;
        number(p, "z", false)?;
        let x = p["x"].as_f64().unwrap();
        ensure!(x >= last, "Points must be sorted by x");
        last = x;
        if !p["z"].is_null() {
            ensure!(
                p["z"].as_f64().unwrap() >= 0.0,
                "Bubble sizes must be nonnegative"
            );
        }
    }
    Ok(())
}
pub fn validate(v: &Value) -> Result<()> {
    ensure!(v.to_string().len() <= 160000, "Panel is too large");
    text(v, "key", 100)?;
    text(v, "title", 200)?;
    text(v, "source", 300)?;
    text(v, "as_of", 100)?;
    url(v, "source_url", false)?;
    ensure!(
        v["description"].is_null() || v["description"].as_str().is_some_and(|s| s.len() <= 4000),
        "Description too long"
    );
    match v["kind"].as_str() {
        Some("shopping") => {
            let products = v["products"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("products required"))?;
            ensure!(
                (1..=8).contains(&products.len()),
                "Use 1–8 products, best pick first"
            );
            let mut ids = std::collections::HashSet::new();
            for p in products {
                text(p, "id", 100)?;
                ensure!(
                    ids.insert(p["id"].as_str().unwrap()),
                    "Duplicate product ID"
                );
                text(p, "name", 200)?;
                text(p, "merchant", 150)?;
                text(p, "description", 2000)?;
                text(p, "compatibility", 1000)?;
                number(p, "price", false)?;
                if let Some(n) = p["price"].as_f64() {
                    ensure!(n >= 0.0, "Price cannot be negative");
                }
                currency(p)?;
                url(p, "url", true)?;
                url(p, "image_url", false)?;
            }
        }
        Some("finance") => {
            let holdings = v["holdings"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("holdings required"))?;
            ensure!(
                (1..=8).contains(&holdings.len()),
                "Use 1–8 financial widgets"
            );
            for h in holdings {
                text(h, "name", 150)?;
                text(h, "symbol", 40)?;
                currency(h)?;
                for key in ["price", "change_percent", "pnl", "balance"] {
                    number(h, key, false)?;
                }
                points(h, 0)?;
                url(h, "url", false)?;
            }
        }
        Some("chart") => {
            ensure!(
                matches!(v["chart_type"].as_str(), Some("line" | "bar" | "scatter")),
                "Choose line, bar or scatter"
            );
            text(v, "x_label", 100)?;
            text(v, "y_label", 100)?;
            ensure!(
                v["x_type"].is_null() || matches!(v["x_type"].as_str(), Some("number" | "time")),
                "x_type must be number or time"
            );
            let series = v["series"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("series required"))?;
            ensure!((1..=6).contains(&series.len()), "Use 1–6 series");
            for s in series {
                text(s, "name", 100)?;
                points(s, 1)?;
            }
            if !v["z_label"].is_null() {
                text(v, "z_label", 100)?;
            }
        }
        _ => crate::workflow_panels::validate(v)?,
    };
    Ok(())
}
pub fn record(c: &Connection, seq: i64) -> Result<Value> {
    let row: Option<(String, String, i64)> = c
        .query_row(
            "SELECT id,body,revision FROM visual_panels WHERE message_seq=?",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((id, body, revision)) = row {
        let mut value: Value = serde_json::from_str(&body)?;
        value["id"] = json!(id);
        value["revision"] = json!(revision);
        crate::workflow_panels::decorate(c, seq, &mut value)?;
        Ok(value)
    } else {
        Ok(Value::Null)
    }
}
pub fn save(db: &Db, run: &Run, v: &Value) -> Result<Value> {
    validate(v)?;
    ensure!(
        !run.chat_id.starts_with("server-"),
        "Visual panels currently support workspace chats; use a text summary in cross-account server chats"
    );
    ensure!(
        db.chat(&run.chat_id)?.members.contains(&run.bot_id),
        "Conversation membership required"
    );
    let mut c = db.0.lock().unwrap();
    let tx = c.transaction()?;
    let old:Option<(String,i64,i64)>=tx.query_row("SELECT id,revision,message_seq FROM visual_panels WHERE bot_id=? AND chat_id=? AND panel_key=?",params![run.bot_id,run.chat_id,v["key"].as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    crate::workflow_panels::validate_refs(&tx, run, v)?;
    let revision = old.as_ref().map(|r| r.1).unwrap_or(0);
    ensure!(
        v["expected_revision"].as_i64() == Some(revision),
        "Panel changed: read it with visual_panel_read and merge before updating"
    );
    let summary = format!(
        "{}\n{}\nSource: {} · {}\nSaved panel key: {}",
        v["title"].as_str().unwrap(),
        v["description"].as_str().unwrap_or(""),
        v["source"].as_str().unwrap(),
        v["as_of"].as_str().unwrap(),
        v["key"].as_str().unwrap()
    );
    let (id, seq) = if let Some((id, _, seq)) = old {
        tx.execute(
            "UPDATE chat_messages SET body=? WHERE seq=?",
            params![summary, seq],
        )?;
        (id, seq)
    } else {
        tx.execute("INSERT INTO chat_messages(chat_id,sender,body,kind,run_id,created) VALUES(?,?,?,'visual_panel',?,?)",params![run.chat_id,run.bot_id,summary,run.id,db::now()])?;
        (db::id(), tx.last_insert_rowid())
    };
    tx.execute("INSERT INTO visual_panels VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body,revision=excluded.revision,run_id=excluded.run_id",params![id,run.bot_id,run.chat_id,v["key"].as_str(),v.to_string(),revision+1,run.id,seq])?;
    tx.commit()?;
    Ok(
        json!({"id":id,"revision":revision+1,"message_seq":seq,"displayed":true,"instructions":"Panel displayed in chat. Do not duplicate it with a Markdown table. Selection and source links do not authorize purchases or trades."}),
    )
}
pub fn read(db: &Db, run: &Run, key: &str) -> Result<Value> {
    ensure!(
        db.chat(&run.chat_id)?.members.contains(&run.bot_id),
        "Conversation membership required"
    );
    let c = db.0.lock().unwrap();
    let seq: i64 = c.query_row(
        "SELECT message_seq FROM visual_panels WHERE bot_id=? AND chat_id=? AND panel_key=?",
        params![run.bot_id, run.chat_id, key],
        |r| r.get(0),
    )?;
    record(&c, seq)
}
pub fn schema() -> Value {
    let point = json!({"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"z":{"type":"number","minimum":0},"label":{"type":"string"}},"required":["x","y"]});
    let points = json!({"type":"array","maxItems":500,"items":point});
    let mut schema = json!({"key":{"type":"string","maxLength":100},"expected_revision":{"type":"integer","minimum":0},"kind":{"type":"string","enum":["shopping","finance","chart","project","review","schedule","sources","upload","monitor"]},"title":{"type":"string","maxLength":200},"description":{"type":"string","maxLength":4000},"source":{"type":"string","maxLength":300},"source_url":{"type":"string"},"as_of":{"type":"string","maxLength":100},"chart_type":{"type":"string","enum":["line","bar","scatter"]},"x_type":{"type":"string","enum":["number","time"]},"x_label":{"type":"string"},"y_label":{"type":"string"},"z_label":{"type":"string"},
 "products":{"type":"array","maxItems":8,"items":{"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"},"merchant":{"type":"string"},"description":{"type":"string"},"compatibility":{"type":"string"},"price":{"type":"number"},"currency":{"type":"string"},"url":{"type":"string"},"image_url":{"type":"string"}},"required":["id","name","merchant","description","compatibility","currency","url"]}},
 "holdings":{"type":"array","maxItems":8,"items":{"type":"object","properties":{"name":{"type":"string"},"symbol":{"type":"string"},"currency":{"type":"string"},"price":{"type":"number"},"balance":{"type":"number"},"change_percent":{"type":"number"},"pnl":{"type":"number"},"url":{"type":"string"},"points":points},"required":["name","symbol","currency","points"]}},
 "series":{"type":"array","maxItems":6,"items":{"type":"object","properties":{"name":{"type":"string"},"points":points},"required":["name","points"]}}});
    schema.as_object_mut().unwrap().extend(
        crate::workflow_panels::schema()
            .as_object()
            .unwrap()
            .clone(),
    );
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    fn panel() -> Value {
        json!({"key":"comparison","expected_revision":0,"kind":"shopping","title":"Compatible hubs","source":"Merchant listing","as_of":"2026-09-20","products":[{"id":"one","name":"USB hub","merchant":"Example","description":"Two ports","compatibility":"Verify laptop support","price":29.0,"currency":"USD","url":"https://example.com/hub"}]})
    }
    #[test]
    fn panels_update_one_message_preserve_revision_and_transfer() {
        let db = Db::open(":memory:").unwrap();
        let b = crate::tests::bot(&db, "codex");
        let id = db.queue(&b.id, "Find a hub", 0).unwrap();
        let run = db.run(&id).unwrap();
        let mut p = panel();
        let first = save(&db, &run, &p).unwrap();
        assert!(save(&db, &run, &p).is_err());
        p["expected_revision"] = json!(1);
        p["products"][0]["price"] = json!(25);
        let second = save(&db, &run, &p).unwrap();
        assert_eq!(first["message_seq"], second["message_seq"]);
        assert_eq!(second["revision"], 2);
        let library = db
            .artifact_page(
                &b.id,
                &crate::artifact_library::PageQuery {
                    kind: "visual".into(),
                    before: String::new(),
                },
            )
            .unwrap();
        assert_eq!(library["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            db.artifact_record(&b.id, "visual", first["id"].as_str().unwrap())
                .unwrap()["revision"],
            2
        );
        let messages = db.chat_messages(&run.chat_id).unwrap();
        let cards: Vec<_> = messages
            .iter()
            .filter(|m| m["kind"] == "visual_panel")
            .collect();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["visual_panel"]["products"][0]["price"], 25);
        let other = crate::tests::bot(&db, "codex");
        let other_id = db.queue(&other.id, "Hello", 0).unwrap();
        let other_run = db.run(&other_id).unwrap();
        assert!(read(&db, &other_run, "comparison").is_err());
        db.finish(&id, "completed", "Shown", "").unwrap();
        db.finish(&other_id, "completed", "Hello", "").unwrap();
        let package = db.prepare_transfer(&db::id(), "Panels").unwrap();
        let to = Db::open(":memory:").unwrap();
        to.import_transfer(&package).unwrap();
        assert_eq!(read(&to, &run, "comparison").unwrap()["revision"], 2);
    }
    #[test]
    fn panels_reject_bad_links_invalid_data_and_unbounded_inputs() {
        let mut p = panel();
        p["products"][0]["url"] = json!("javascript:alert(1)");
        assert!(validate(&p).is_err());
        p = panel();
        p["products"][0]["price"] = json!(-2);
        assert!(validate(&p).is_err());
        p = panel();
        p["products"][0]["image_url"] = json!("https://user:password@example.com/a");
        assert!(validate(&p).is_err());
        p = json!({"key":"chart","kind":"chart","title":"Trend","source":"Observed data","as_of":"2026-09-20","chart_type":"scatter","x_label":"Time","y_label":"Value","series":[{"name":"A","points":[{"x":2,"y":5},{"x":1,"y":6}]}]});
        assert!(validate(&p).is_err());
        p["series"][0]["points"] = json!([{"x":1,"y":-5,"z":0},{"x":2,"y":6,"z":12}]);
        assert!(validate(&p).is_ok());
        p["series"][0]["points"] = json!(vec![json!({"x":1,"y":1}); 501]);
        assert!(validate(&p).is_err());
        p = json!({"key":"finance","kind":"finance","title":"Balances","source":"User-provided snapshot","as_of":"2026-09-20","holdings":[{"name":"Checking","symbol":"Cash","currency":"USD","balance":1200,"points":[]}]});
        assert!(validate(&p).is_ok());
        assert!(p["holdings"][0]["pnl"].is_null());
    }
}
